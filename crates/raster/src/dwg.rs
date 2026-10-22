//! DWG (R13 and later) model-space geometry, read with `acadrust` and expanded with the DXF output
//! helpers, so both formats share block expansion, layers, colours, text and extents.

use std::f64::consts::TAU;
use std::io::Cursor;

use acadrust::entities::{AttributeEntity, BoundaryEdge, BoundaryPath, EntityCommon, LeaderPathType};
use acadrust::types::{Color, Handle, Vector2, Vector3};
use acadrust::{CadDocument, DwgReader, EntityType};

use crate::dxf::{self, parse, Affine, Ctx, HatchLine, Layer, LeaderLine, LeaderRoot, LeaderText, MultiLeader, Parent, Pen, World, P};

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
            Layer { name: l.name.clone(), color: rgb.unwrap_or_else(|| dxf::aci(index)), visible: l.is_visible(), linetype: l.line_type.clone() }
        })
        .collect();
    let units = dxf::units_name(doc.header.insertion_units.into());
    let mut info = vec![("DWG version".to_string(), dxf::version(Some(code)))];
    info.extend(dxf::summary(units, &layers, doc.block_records.names(), doc.model_space_entities().count()));

    let mut b = Builder { doc: &doc, world: World::new(layers) };
    // Complex linetypes (shapes or text in the gaps) are drawn solid.
    for lt in doc.line_types.iter().filter(|lt| lt.elements.iter().all(|e| e.complex.is_none())) {
        b.world.add_linetype(&lt.name, lt.elements.iter().map(|e| e.length * doc.header.linetype_scale));
    }
    for e in doc.model_space_entities() {
        b.entity(e, &Ctx::ROOT);
    }
    b.world.finish(units, info)
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

/// Draws a TEXT-like attribute (on an INSERT, or standalone).
fn attribute(w: &mut World, a: &AttributeEntity, m: &Affine, pen: Pen) {
    let (h, v) = (a.horizontal_alignment as i64, a.vertical_alignment as i64);
    let (pos, rotation, align) = dxf::text_anchor(h, v, xy(a.insertion_point), xy(a.alignment_point), a.rotation);
    w.push_text(pos, rotation, a.height, a.width_factor, align, parse::text_codes(&a.value), &m.then(&Affine::extrusion(a.normal.z)), pen);
}

struct Builder<'a> {
    doc: &'a CadDocument,
    world: World<'a>,
}

impl<'a> Builder<'a> {
    fn entity(&mut self, e: &'a EntityType, ctx: &Ctx<'a>) {
        let Some((parent, pen)) = self.style(e.common(), ctx) else { return };
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
            EntityType::Polyline3D(p) => {
                let verts: Vec<_> = p.vertices.iter().filter(|v| v.flags & 16 == 0).map(|v| (xy(v.position), 0.0, [0.0; 2])).collect();
                w.polyline(&verts, p.flags.closed, &m, pen);
            }
            // SOLID and TRACE list their corners in zig-zag order.
            EntityType::Solid(s) => {
                let pts = [s.first_corner, s.second_corner, s.fourth_corner, s.third_corner, s.first_corner];
                w.path(pts.map(xy).to_vec(), &ocs(s.normal), pen);
            }
            EntityType::Face3D(f) => {
                let pts = [f.first_corner, f.second_corner, f.third_corner, f.fourth_corner, f.first_corner];
                w.path(pts.map(xy).to_vec(), &m, pen);
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
                w.push_text(pos, rotation, t.height, t.width_factor, align, parse::text_codes(&t.value), &ocs(t.normal), pen);
            }
            EntityType::AttributeEntity(a) => attribute(w, a, &m, pen),
            EntityType::MText(t) => {
                let align = dxf::mtext_align(t.attachment_point as i64);
                w.mtext(xy(t.insertion_point), t.rotation, t.height, t.rectangle_width, align, parse::mtext_plain(&t.value), &m, pen);
            }
            EntityType::Insert(i) => {
                let base = self.doc.block_records.get(&i.block_name).map_or([0.0; 2], |b| xy(b.base_point));
                let grid = (i.column_count.into(), i.row_count.into());
                let cells = dxf::insert_cells(&m, Affine::extrusion(i.normal.z), xy(i.insert_point), i.rotation, [i.x_scale(), i.y_scale()], base, grid, [i.column_spacing, i.row_spacing]);
                for cell in cells {
                    self.expand(&i.block_name, cell, parent, ctx);
                }
                for a in i.attributes.iter().filter(|a| !a.flags.invisible) {
                    if let Some((_, pen)) = self.style(&a.common, ctx) {
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
                // A gradient is filled with its first colour.
                let gradient = h.gradient_color.colors.first().filter(|_| h.gradient_color.enabled).map(|c| color(&c.color));
                let pen = match gradient {
                    Some((index, rgb)) => Pen { color: rgb.unwrap_or_else(|| dxf::aci(index)), ..pen },
                    None => pen,
                };
                let solid = h.is_solid || gradient.is_some();
                w.hatch(rings, (!solid).then_some(&lines[..]), &ocs(h.normal), pen);
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
            // A dimension's lines, arrows and text are in an anonymous block, already in world coordinates.
            EntityType::Dimension(d) => self.expand(&d.base().block_name, m, parent, ctx),
            // Attribute definitions show as attributes on the INSERT instead.
            EntityType::AttributeDefinition(_) => {}
            other => w.skip(other.as_entity().entity_type()),
        }
    }

    /// What the entity passes on as a parent, and its pen; None if it is invisible.
    fn style(&mut self, c: &'a EntityCommon, ctx: &Ctx<'a>) -> Option<(Parent<'a>, Pen)> {
        if c.invisible {
            return None;
        }
        let own = if c.layer.is_empty() { "0" } else { c.layer.as_str() };
        let (index, rgb) = color(&c.color);
        Some(dxf::style(&mut self.world, own, rgb, index, (&c.linetype, c.linetype_scale), ctx))
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
