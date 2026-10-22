//! DWG (R13 and later) model-space geometry, read with `acadrust` and expanded with the DXF output
//! helpers, so both formats share block expansion, layers, colours, text and extents.

use std::f64::consts::TAU;
use std::io::Cursor;

use acadrust::entities::{AttributeEntity, Body, BoundaryEdge, BoundaryPath, Dimension, EntityCommon, LeaderPathType, Region, Solid3D};
use acadrust::tables::{LineType, LineTypeComplexContent};
use acadrust::types::{Color, Handle, Vector2, Vector3};
use acadrust::{CadDocument, DwgReader, EntityType};

use crate::dxf::dim::{self, Dim, DimStyle, DimText};
use crate::dxf::{self, parse, Affine, Ctx, HatchLine, Layer, LeaderLine, LeaderRoot, LeaderText, LinetypeElement, LinetypeShape, MultiLeader, Parent, Pen, World, P};

pub fn decode(data: &[u8]) -> Result<dxf::Drawing, String> {
    let code = data.get(..6).and_then(|v| std::str::from_utf8(v).ok()).filter(|v| v.starts_with("AC"));
    let Some(code) = code else { return Err("Not a DWG file".into()) };
    if code < "AC1012" {
        return Err(format!("{} is not supported; only DWG R13 and later can be opened", dxf::version(Some(code))));
    }
    let doc = DwgReader::from_stream(Cursor::new(data)).read().map_err(|e| format!("Cannot read DWG: {e}"))?;

    let layers: Vec<Layer> = doc
        .layers
        .iter()
        .map(|l| {
            let (index, rgb) = color(&l.color);
            Layer { name: l.name.clone(), color: rgb.unwrap_or_else(|| dxf::aci(index)), visible: l.is_visible(), linetype: l.line_type.clone(), weight: l.line_weight.value() }
        })
        .collect();
    let units = dxf::units_name(doc.header.insertion_units.into());
    let mut info = vec![("DWG version".to_string(), dxf::version(Some(code)))];
    info.extend(dxf::summary(units, &layers, doc.block_records.names(), doc.model_space_entities().count()));

    let h = &doc.header;
    let dim_style = DimStyle {
        arrow: h.dim_arrow_size,
        text: h.dim_text_height,
        ext_offset: h.dim_ext_line_offset,
        ext_beyond: h.dim_ext_line_extension,
        gap: h.dim_line_gap,
        above: h.dim_text_above != 0,
        horizontal: h.dim_text_inside_horizontal,
        lfac: h.dim_linear_scale,
        dec: h.dim_decimal_places.into(),
        adec: h.dim_angular_decimal_places.into(),
        zin: h.dim_zero_suppression.into(),
        sep: h.dim_decimal_separator,
        post: h.dim_post.clone(),
        scale: h.dim_scale,
    };
    let mut b = Builder { doc: &doc, world: World::new(layers), dim_style };
    for lt in doc.line_types.iter() {
        if let Some(elements) = linetype_elements(&doc, lt) {
            b.world.add_linetype(&lt.name, elements.iter().map(|e| e.scaled(doc.header.linetype_scale)).collect());
        }
    }
    for e in doc.model_space_entities() {
        b.entity(e, &Ctx::ROOT);
    }
    b.world.finish(units, info)
}

/// What an entity with properties `c` passes on as a parent, and its pen; None if it is invisible.
fn style<'a>(w: &mut World, c: &'a EntityCommon, ctx: &Ctx<'a>) -> Option<(Parent<'a>, Pen)> {
    if c.invisible {
        return None;
    }
    let own = if c.layer.is_empty() { "0" } else { c.layer.as_str() };
    let (index, rgb) = color(&c.color);
    Some(dxf::style(w, own, rgb, index, (&c.linetype, c.linetype_scale), c.line_weight.value().into(), ctx))
}

fn xy(v: Vector3) -> P {
    [v.x, v.y]
}

/// A handle as upper-case hex, as DXF writes it; None if null.
fn hex(h: Option<Handle>) -> Option<String> {
    h.filter(|h| !h.is_null()).map(|h| format!("{h:X}"))
}

/// `pen` in colour `c`, unless that is BYLAYER or BYBLOCK (the entity's own colour).
fn recolor(c: &Color, pen: Pen) -> Pen {
    match color(c) {
        (_, Some(rgb)) => Pen { color: rgb, ..pen },
        (index @ 1..=255, None) => Pen { color: dxf::aci(index), ..pen },
        _ => pen,
    }
}

fn v2(v: Vector2) -> P {
    [v.x, v.y]
}

/// The elements of a linetype; None if it has text, or shapes that are not built in (see
/// `dxf::shapes`), which are drawn solid instead.
fn linetype_elements(doc: &CadDocument, lt: &LineType) -> Option<Vec<LinetypeElement>> {
    lt.elements
        .iter()
        .map(|e| {
            let Some(c) = &e.complex else { return Some(LinetypeElement::plain(e.length)) };
            let LineTypeComplexContent::Shape { shape_number } = c.content else { return None };
            let file = doc.text_styles.iter().find(|s| s.handle == c.style_handle).map_or("", |s| s.font_file.as_str());
            let strokes = dxf::shapes::strokes(file, shape_number.into())?;
            let shape = LinetypeShape { strokes, scale: c.scale, rotation: c.rotation, absolute: c.absolute_rotation, offset: c.offset };
            Some(LinetypeElement { length: e.length, shape: Some(shape) })
        })
        .collect()
}

/// A hatch boundary path as one ring of points.
fn boundary(path: &BoundaryPath) -> Vec<P> {
    let mut ring = Vec::new();
    for edge in &path.edges {
        match edge {
            BoundaryEdge::Line(l) => ring.extend([v2(l.start), v2(l.end)]),
            BoundaryEdge::CircularArc(a) => ring.extend(dxf::arc_edge_points(v2(a.center), [a.radius, 0.0], 1.0, a.start_angle, a.end_angle, a.counter_clockwise)),
            BoundaryEdge::EllipticArc(e) => ring.extend(dxf::arc_edge_points(v2(e.center), v2(e.major_axis_endpoint), e.minor_axis_ratio, e.start_angle, e.end_angle, e.counter_clockwise)),
            BoundaryEdge::Spline(s) => {
                let ctrl: Vec<_> = s.control_points.iter().map(|&p| xy(p)).collect();
                let weights: Vec<_> = if s.rational { s.control_points.iter().map(|p| p.z).collect() } else { Vec::new() };
                let fit: Vec<_> = s.fit_points.iter().map(|&p| v2(p)).collect();
                ring.extend(dxf::spline_points(s.degree.max(1) as usize, &ctrl, &s.knots, &weights, &fit));
            }
            // Vertex z is the bulge.
            BoundaryEdge::Polyline(p) => {
                let verts: Vec<_> = p.vertices.iter().map(|v| ([v.x, v.y], v.z, [0.0; 2])).collect();
                ring.extend(dxf::polyline_points(&verts, p.is_closed));
            }
        }
    }
    ring
}

/// ACI index (0 BYBLOCK, 256 BYLAYER) and true colour, as `dxf::style` takes them.
fn color(c: &Color) -> (i64, Option<u32>) {
    match *c {
        Color::ByBlock => (0, None),
        Color::ByLayer | Color::None => (256, None),
        Color::Index(i) => (i.into(), None),
        Color::Rgb { r, g, b } => (256, Some(u32::from_be_bytes([0, r, g, b]))),
    }
}

/// A polyline vertex's start and end widths, or the polyline's own (constant or default) widths if it has none.
fn widths(own: [f64; 2], default: [f64; 2]) -> [f64; 2] {
    if own == [0.0; 2] { default } else { own }
}

/// Draws a TEXT-like attribute (on an INSERT, or standalone), or attribute definition with `value`.
macro_rules! attribute {
    ($w:expr, $a:expr, $value:expr, $m:expr, $pen:expr) => {{
        let a = $a;
        let (h, v) = (a.horizontal_alignment as i64, a.vertical_alignment as i64);
        let (pos, rotation, align) = dxf::text_anchor(h, v, xy(a.insertion_point), xy(a.alignment_point), a.rotation);
        let m = $m.then(&Affine::extrusion(a.normal.z)).then(&dxf::text_shape(a.text_generation_flags.into(), a.oblique_angle, pos, rotation));
        $w.push_text(pos, rotation, a.height, a.width_factor, align, parse::text_rich($value), &m, $pen);
    }};
}

fn attribute(w: &mut World, a: &AttributeEntity, m: &Affine, pen: Pen) {
    attribute!(w, a, &a.value, m, pen)
}

/// A dimension style's variables.
fn dim_style(s: &acadrust::tables::DimStyle) -> DimStyle {
    DimStyle {
        arrow: s.dimasz,
        text: s.dimtxt,
        ext_offset: s.dimexo,
        ext_beyond: s.dimexe,
        gap: s.dimgap,
        above: s.dimtad != 0,
        horizontal: s.dimtih,
        lfac: s.dimlfac,
        dec: s.dimdec.into(),
        adec: s.dimadec.into(),
        zin: s.dimzin.into(),
        sep: char::from_u32(s.dimdsep as u32).filter(|&c| c > ' ').unwrap_or('.'),
        post: s.dimpost.clone(),
        scale: s.dimscale,
    }
}

/// A dimension's geometry and text.
fn dimension(d: &Dimension) -> (Dim, DimText<'_>) {
    let geometry = match d {
        Dimension::Linear(d) => Dim::Linear { ext1: xy(d.first_point), ext2: xy(d.second_point), line: xy(d.definition_point), angle: Some(d.rotation) },
        Dimension::Aligned(d) => Dim::Linear { ext1: xy(d.first_point), ext2: xy(d.second_point), line: xy(d.definition_point), angle: None },
        Dimension::Radius(d) => Dim::Radius { centre: xy(d.angle_vertex), on_curve: xy(d.definition_point) },
        Dimension::Diameter(d) => Dim::Diameter { a: xy(d.angle_vertex), b: xy(d.definition_point) },
        Dimension::Angular2Ln(d) => Dim::Angular2 { l1: [xy(d.first_point), xy(d.second_point)], l2: [xy(d.angle_vertex), xy(d.definition_point)], arc: xy(d.dimension_arc) },
        Dimension::Angular3Pt(d) => Dim::Angular3 { vertex: xy(d.angle_vertex), a: xy(d.first_point), b: xy(d.second_point), arc: xy(d.definition_point) },
        Dimension::Ordinate(d) => Dim::Ordinate { origin: xy(d.definition_point), feature: xy(d.feature_location), leader: xy(d.leader_endpoint), x: d.is_ordinate_type_x },
        Dimension::Arc(d) => Dim::ArcLength { centre: xy(d.center_point), a: xy(d.first_extension_point), b: xy(d.second_extension_point), arc: xy(d.definition_point) },
        Dimension::LargeRadial(d) => Dim::Jogged { centre: xy(d.definition_point), chord: xy(d.chord_point), jog: xy(d.jog_point), shown_centre: xy(d.override_center) },
    };
    let b = d.base();
    // A text point at the origin is a writer's default, not a position.
    let at = Some(xy(b.text_middle_point)).filter(|&p| p != [0.0; 2]);
    let user = b.user_text.as_deref().unwrap_or(&b.text);
    (geometry, DimText { user, at, rotation: Some(b.text_rotation).filter(|&a| a != 0.0) })
}

struct Builder<'a> {
    doc: &'a CadDocument,
    world: World<'a>,
    /// For dimensions whose style is not listed.
    dim_style: DimStyle,
}

impl<'a> Builder<'a> {
    fn entity(&mut self, e: &'a EntityType, ctx: &Ctx<'a>) {
        let Some((parent, pen)) = style(&mut self.world, e.common(), ctx) else { return };
        let (m, w) = (ctx.m, &mut self.world);
        let ocs = |normal: Vector3| m.then(&Affine::extrusion(normal.z));
        match e {
            EntityType::Line(l) => w.path(vec![xy(l.start), xy(l.end)], &m, pen),
            EntityType::Point(p) => w.path(vec![xy(p.location); 2], &m, pen),
            EntityType::Circle(c) => w.circle_arc(xy(c.center), c.radius, 0.0, TAU, &ocs(c.normal), pen),
            EntityType::Arc(a) => w.circle_arc(xy(a.center), a.radius, a.start_angle, dxf::sweep(a.start_angle, a.end_angle), &ocs(a.normal), pen),
            EntityType::Ellipse(el) => {
                let ratio = el.minor_axis_ratio * el.normal.z.signum();
                w.ellipse(xy(el.center), xy(el.major_axis), ratio, el.start_parameter, el.end_parameter, &m, pen);
            }
            EntityType::LwPolyline(p) => {
                let verts: Vec<_> = p.vertices.iter().map(|v| ([v.location.x, v.location.y], v.bulge, widths([v.start_width, v.end_width], [p.constant_width; 2]))).collect();
                w.polyline(&verts, p.is_closed, &ocs(p.normal), pen);
            }
            EntityType::Polyline2D(p) => {
                // Spline frame control points (vertex flag 16) are not part of the curve.
                let verts = p.vertices.iter().filter(|v| v.flags.bits() & 16 == 0);
                let verts: Vec<_> = verts.map(|v| (xy(v.location), v.bulge, widths([v.start_width, v.end_width], [p.start_width, p.end_width]))).collect();
                w.polyline(&verts, p.flags.is_closed(), &ocs(p.normal), pen);
            }
            EntityType::PolyfaceMesh(p) => {
                let verts: Vec<P> = p.vertices.iter().map(|v| xy(v.location)).collect();
                let faces: Vec<[i64; 4]> = p.faces.iter().map(|f| [f.index1, f.index2, f.index3, f.index4].map(i64::from)).collect();
                // Each face has its own layer, colour, linetype and weight; an invisible one draws nothing.
                let pens: Vec<Option<Pen>> = p.faces.iter().map(|f| style(w, &f.common, ctx).map(|s| s.1)).collect();
                for (f, edge) in dxf::polyface_edges(&verts, &faces) {
                    if let Some(pen) = pens[f] {
                        w.path(edge, &m, pen);
                    }
                }
            }
            EntityType::PolygonMesh(p) => {
                let verts: Vec<P> = p.vertices.iter().map(|v| xy(v.location)).collect();
                let (rows, cols) = (p.m_vertex_count.max(0) as usize, p.n_vertex_count.max(0) as usize);
                let closed = (p.flags.bits() & 1 != 0, p.flags.bits() & 32 != 0);
                dxf::mesh_grid(&verts, rows, cols, closed.0, closed.1).into_iter().for_each(|line| w.path(line, &m, pen));
            }
            EntityType::Mesh(mesh) => {
                for face in &mesh.faces {
                    let mut ring: Vec<P> = face.vertices.iter().filter_map(|&i| mesh.vertices.get(i)).map(|&v| xy(v)).collect();
                    ring.extend(ring.first().copied());
                    w.path(ring, &m, pen);
                }
            }
            EntityType::Helix(h) => {
                let s = &h.spline;
                let ctrl: Vec<_> = s.control_points.iter().copied().map(xy).collect();
                let fit: Vec<_> = s.fit_points.iter().copied().map(xy).collect();
                w.path(dxf::spline_points(s.degree.max(1) as usize, &ctrl, &s.knots, &s.weights, &fit), &m, pen);
            }
            EntityType::MLine(ml) => {
                // Each element runs through the vertices, moved along their miters by its first parameter.
                let elements = ml.vertices.iter().map(|v| v.segments.len()).max().unwrap_or(0);
                for e in 0..elements {
                    let mut line: Vec<P> = ml.vertices.iter().map(|v| {
                        let t = v.segments.get(e).and_then(|s| s.parameters.first()).copied().unwrap_or(0.0);
                        xy(v.position + v.miter * t)
                    }).collect();
                    if ml.is_closed() {
                        line.extend(line.first().copied());
                    }
                    w.path(line, &m, pen);
                }
            }
            EntityType::Wipeout(wo) => {
                let clip: Vec<P> = wo.clip_boundary_vertices.iter().map(|&v| v2(v)).collect();
                w.wipeout(dxf::wipeout_ring(xy(wo.insertion_point), xy(wo.u_vector), xy(wo.v_vector), v2(wo.size), &clip), &m, pen.layer);
            }
            EntityType::Tolerance(t) => {
                let style = self.doc.dim_styles.get(&t.dimension_style_name).map_or_else(|| self.dim_style.clone(), dim_style).scaled();
                let dir = if t.direction.x == 0.0 && t.direction.y == 0.0 { [1.0, 0.0] } else { xy(t.direction) };
                dim::tolerance(w, xy(t.insertion_point), dir, &t.text, &style, &m, pen);
            }
            // A table's graphics are an anonymous block (named, or found by its record's handle),
            // placed like an INSERT along the table's direction.
            EntityType::Table(t) => {
                let name = Some(t.block_name.as_str()).filter(|n| !n.is_empty()).or_else(|| self.block_name(&hex(t.block_record_handle)?));
                let Some(name) = name else { return };
                let dir = t.horizontal_direction;
                let at = m.then(&Affine::translate(xy(t.insertion_point))).then(&Affine::rotate(dir.y.atan2(dir.x)));
                self.expand(name, at, parent, ctx);
            }
            EntityType::Polyline3D(p) => {
                let verts: Vec<_> = p.vertices.iter().filter(|v| v.flags & 16 == 0).map(|v| (xy(v.position), 0.0, [0.0; 2])).collect();
                w.polyline(&verts, p.flags.closed, &m, pen);
            }
            EntityType::Solid(s) => w.solid([s.first_corner, s.second_corner, s.third_corner, s.fourth_corner].map(xy), &ocs(s.normal), pen),
            EntityType::Face3D(f) => {
                let pts = [f.first_corner, f.second_corner, f.third_corner, f.fourth_corner, f.first_corner];
                w.path(pts.map(xy).to_vec(), &m, pen);
            }
            EntityType::Region(Region { acis_data, .. }) | EntityType::Solid3D(Solid3D { acis_data, .. }) | EntityType::Body(Body { acis_data, .. }) => {
                for edge in dxf::acis::read(&acis_data.sat_data, &acis_data.sab_data).as_ref().map(dxf::acis::edges).unwrap_or_default() {
                    w.path(edge, &m, pen);
                }
            }
            EntityType::Spline(s) => {
                let ctrl: Vec<_> = s.control_points.iter().copied().map(xy).collect();
                let fit: Vec<_> = s.fit_points.iter().copied().map(xy).collect();
                w.path(dxf::spline_points(s.degree.max(1) as usize, &ctrl, &s.knots, &s.weights, &fit), &m, pen);
            }
            EntityType::Text(t) => {
                let second = t.alignment_point.unwrap_or(t.insertion_point);
                let (h, v) = (t.horizontal_alignment as i64, t.vertical_alignment as i64);
                let (pos, rotation, align) = dxf::text_anchor(h, v, xy(t.insertion_point), xy(second), t.rotation);
                let m = ocs(t.normal).then(&dxf::text_shape(t.generation_flags.into(), t.oblique_angle, pos, rotation));
                w.push_text(pos, rotation, t.height, t.width_factor, align, parse::text_rich(&t.value), &m, pen);
            }
            EntityType::AttributeEntity(a) => attribute(w, a, &m, pen),
            // An attribute definition shows its tag outside blocks; inside one only a constant one shows, its value.
            EntityType::AttributeDefinition(a) if !a.flags.invisible => match (ctx.depth, a.flags.constant) {
                (0, _) => attribute!(w, a, &a.tag, m, pen),
                (_, true) => attribute!(w, a, &a.default_value, m, pen),
                _ => {}
            },
            EntityType::MText(t) => {
                let align = dxf::mtext_align(t.attachment_point as i64);
                let start = w.text_count();
                w.mtext(xy(t.insertion_point), t.rotation, t.height, t.rectangle_width, align, parse::mtext_rich(&t.value, t.height), &m, pen);
                // Background mask: flag 1 in its own colour, 2 in the paper's.
                if t.background_fill_flags & 3 != 0 {
                    let color = if t.background_fill_flags & 2 != 0 { dxf::BACKGROUND } else { recolor(&t.background_color, pen).color };
                    w.mask_texts(start, color, (t.background_scale - 1.0) * t.height, &m);
                }
            }
            EntityType::Insert(i) => {
                let base = self.doc.block_records.get(&i.block_name).map_or([0.0; 2], |b| xy(b.base_point));
                let grid = (i.column_count.into(), i.row_count.into());
                let cells = dxf::insert_cells(&m, Affine::extrusion(i.normal.z), xy(i.insert_point), i.rotation, [i.x_scale(), i.y_scale()], base, grid, [i.column_spacing, i.row_spacing]);
                for cell in cells {
                    self.expand(&i.block_name, cell, parent, ctx);
                }
                for a in i.attributes.iter().filter(|a| !a.flags.invisible) {
                    if let Some((_, pen)) = style(&mut self.world, &a.common, ctx) {
                        attribute(&mut self.world, a, &m, pen);
                    }
                }
            }
            EntityType::Ray(r) => w.ray(xy(r.base_point), xy(r.direction), false, &m, pen),
            EntityType::XLine(x) => w.ray(xy(x.base_point), xy(x.direction), true, &m, pen),
            EntityType::Leader(l) => {
                // Only R13/R14 leaders carry their own arrow size; later ones take their dimension style's.
                let (header, style) = (&self.doc.header, self.doc.dim_styles.get(&l.dimension_style));
                let dimasz = Some(l.arrow_size).filter(|&s| s > 0.0).or(style.map(|s| s.dimasz)).unwrap_or(header.dim_arrow_size);
                let scale = style.map_or(header.dim_scale, |s| s.dimscale);
                let size = if l.arrow_enabled { dxf::arrow_size(dimasz, scale) } else { 0.0 };
                let spline = l.path_type == LeaderPathType::Spline;
                w.leader(l.vertices.iter().map(|&v| xy(v)).collect(), spline, size, &m, pen);
            }
            EntityType::Hatch(h) => {
                let rings = h.paths.iter().map(boundary).collect();
                let lines: Vec<_> = h.pattern.lines.iter().map(|l| HatchLine { angle: l.angle, base: v2(l.base_point), offset: v2(l.offset), dashes: l.dash_lengths.clone() }).collect();
                let g = &h.gradient_color;
                let colors: Vec<u32> = g.colors.iter().map(|c| recolor(&c.color, pen).color).collect();
                match dxf::gradient_colors(&colors, g.is_single_color, g.color_tint).filter(|_| g.enabled) {
                    Some((c1, c2)) => w.gradient(rings, &g.name, g.angle, c1, c2, &ocs(h.normal), pen),
                    None => w.hatch(rings, (!h.is_solid).then_some(&lines[..]), &ocs(h.normal), pen),
                }
            }
            EntityType::MultiLeader(ml) => {
                let c = &ml.context;
                let roots = c.leader_roots.iter().map(|r| LeaderRoot {
                    connection: xy(r.connection_point),
                    direction: xy(r.direction),
                    landing: r.landing_distance,
                    lines: r.lines.iter().map(|l| LeaderLine { points: l.points.iter().map(|&p| xy(p)).collect(), arrow: hex(l.arrowhead_handle) }).collect(),
                });
                let text = c.has_text_contents.then(|| LeaderText {
                    value: c.text_string.clone(),
                    at: xy(c.text_location),
                    angle: c.text_direction.y.atan2(c.text_direction.x),
                    height: c.text_height,
                    column: (c.text_attachment_point as i16 - 1).clamp(0, 2) as u8,
                });
                let content = hex(c.block_content_handle).filter(|_| c.has_block_contents);
                let block = content.map(|h| (h, xy(c.block_content_location), xy(c.block_content_scale), c.block_rotation));
                let leader = MultiLeader { roots: roots.collect(), dogleg: ml.enable_dogleg, arrow_size: c.arrowhead_size, arrow: hex(ml.arrowhead_handle), text, block };
                self.multileader(&leader, m, parent, recolor(&ml.line_color, pen), recolor(&c.text_color, pen), ctx);
            }
            // A dimension's lines, arrows and text are in an anonymous block, already in world
            // coordinates. Without one it is drawn from its definition points.
            EntityType::Dimension(d) => {
                let name = &d.base().block_name;
                if self.doc.entities_in_block(name).next().is_some() {
                    return self.expand(name, m, parent, ctx);
                }
                let style = self.doc.dim_styles.get(&d.base().style_name).map_or_else(|| self.dim_style.clone(), dim_style).scaled();
                let (geometry, text) = dimension(d);
                dim::draw(&mut self.world, &geometry, &text, &style, &m, pen);
            }
            EntityType::AttributeDefinition(_) => {}
            other => w.skip(other.as_entity().entity_type()),
        }
    }


    /// Draws a multileader: its lines and text, arrowheads (blocks or the default) and block content.
    fn multileader(&mut self, ml: &MultiLeader, m: Affine, parent: Parent<'a>, line: Pen, text: Pen, ctx: &Ctx<'a>) {
        for (at, arrow) in self.world.multileader(ml, &m, line, text) {
            match arrow.and_then(|h| self.block_name(&h)) {
                Some(name) => self.expand(name, at, Parent { color: line.color, ..parent }, ctx),
                None => self.world.arrowhead(&at, line),
            }
        }
        let Some((handle, at, scale, rotation)) = &ml.block else { return };
        let Some(name) = self.block_name(handle) else { return };
        let base = self.doc.block_records.get(name).map_or([0.0; 2], |b| xy(b.base_point));
        for cell in dxf::insert_cells(&m, Affine::IDENTITY, *at, *rotation, *scale, base, (1, 1), [0.0; 2]) {
            self.expand(name, cell, parent, ctx);
        }
    }

    /// Name of the block whose record has handle `hex`.
    fn block_name(&self, hex: &str) -> Option<&'a str> {
        let doc = self.doc;
        doc.block_records.iter().find(|b| format!("{:X}", b.handle) == hex).map(|b| b.name.as_str())
    }

    /// Draws block `name` with transform `m`, as the child of an INSERT or DIMENSION.
    fn expand(&mut self, name: &str, m: Affine, parent: Parent<'a>, ctx: &Ctx<'a>) {
        if ctx.depth >= dxf::MAX_DEPTH {
            return;
        }
        let inner = Ctx { m, parent: Some(parent), depth: ctx.depth + 1 };
        let doc = self.doc;
        for e in doc.entities_in_block(name) {
            self.entity(e, &inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use acadrust::entities::{Leader, Ray, XLine};
    use acadrust::{CadDocument, DwgWriter, EntityType, Line};

    use super::*;

    #[test]
    fn reads_line() {
        let mut doc = CadDocument::new();
        doc.add_entity(EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 100.0, 50.0, 0.0))).unwrap();
        let d = decode(&DwgWriter::write_to_vec(&doc).unwrap()).unwrap();
        assert_eq!((d.width, d.height), (100.0, 50.0));
        assert_eq!(d.paths.len(), 1);
        assert!(d.info[0].0 == "DWG version" && d.info[0].1.starts_with("AC"), "{:?}", d.info);
    }

    #[test]
    fn reads_ray_xline_and_leader() {
        let v = |x: f64, y: f64| Vector3::new(x, y, 0.0);
        let mut doc = CadDocument::new();
        doc.add_entity(EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 10.0, 10.0, 0.0))).unwrap();
        doc.add_entity(EntityType::Ray(Ray::new(v(5.0, 5.0), v(1.0, 0.0)))).unwrap();
        doc.add_entity(EntityType::XLine(XLine::new(v(5.0, 5.0), v(0.0, 1.0)))).unwrap();
        doc.add_entity(EntityType::Leader(Leader::from_vertices(vec![v(0.0, 0.0), v(4.0, 4.0)]))).unwrap();
        let d = decode(&DwgWriter::write_to_vec(&doc).unwrap()).unwrap();
        assert_eq!((d.width, d.height), (10.0, 10.0));
        // Line, leader, then the ray and xline, which are added once the extents are known.
        assert_eq!(d.paths.len(), 4);
        assert_eq!(d.fills.len(), 1, "leader arrowhead");
    }

    #[test]
    fn rejects_old_and_non_dwg() {
        assert!(decode(b"AC1009\0\0").err().unwrap().contains("R13"));
        assert!(decode(b"hello").is_err());
    }
}
