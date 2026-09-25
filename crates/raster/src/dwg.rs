//! DWG (R13 and later) model-space geometry, read with `acadrust` and expanded with the DXF output
//! helpers, so both formats share block expansion, layers, colours, text and extents.

use std::f64::consts::TAU;
use std::io::Cursor;

use acadrust::entities::{AttributeEntity, EntityCommon};
use acadrust::types::{Color, Vector3};
use acadrust::{CadDocument, DwgReader, EntityType};

use crate::dxf::{self, parse, Affine, Ctx, Layer, Pen, World, P};

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
            Layer { name: l.name.clone(), color: rgb.unwrap_or_else(|| dxf::aci(index)), visible: l.is_visible() }
        })
        .collect();
    let units = dxf::units_name(doc.header.insertion_units.into());
    let mut info = vec![("DWG version".to_string(), dxf::version(Some(code)))];
    info.extend(dxf::summary(units, &layers, doc.block_records.names(), doc.model_space_entities().count()));

    let mut b = Builder { doc: &doc, world: World::new(layers) };
    for e in doc.model_space_entities() {
        b.entity(e, &Ctx::ROOT);
    }
    b.world.finish(units, info)
}

fn xy(v: Vector3) -> P {
    [v.x, v.y]
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
        let Some((layer, pen)) = self.style(e.common(), ctx) else { return };
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
                let verts: Vec<_> = p.vertices.iter().map(|v| ([v.location.x, v.location.y], v.bulge)).collect();
                w.polyline(&verts, p.is_closed, &ocs(p.normal), pen);
            }
            EntityType::Polyline2D(p) => {
                // Spline frame control points (vertex flag 16) are not part of the curve.
                let verts: Vec<_> = p.vertices.iter().filter(|v| v.flags.bits() & 16 == 0).map(|v| (xy(v.location), v.bulge)).collect();
                w.polyline(&verts, p.flags.is_closed(), &ocs(p.normal), pen);
            }
            EntityType::Polyline3D(p) => {
                let verts: Vec<_> = p.vertices.iter().filter(|v| v.flags & 16 == 0).map(|v| (xy(v.position), 0.0)).collect();
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
                w.push_text(xy(t.insertion_point), t.rotation, t.height, 1.0, align, parse::mtext_plain(&t.value), &m, pen);
            }
            EntityType::Insert(i) => {
                let base = self.doc.block_records.get(&i.block_name).map_or([0.0; 2], |b| xy(b.base_point));
                let grid = (i.column_count.into(), i.row_count.into());
                let cells = dxf::insert_cells(&m, Affine::extrusion(i.normal.z), xy(i.insert_point), i.rotation, [i.x_scale(), i.y_scale()], base, grid, [i.column_spacing, i.row_spacing]);
                for cell in cells {
                    self.expand(&i.block_name, cell, layer, pen, ctx);
                }
                for a in i.attributes.iter().filter(|a| !a.flags.invisible) {
                    if let Some((_, pen)) = self.style(&a.common, ctx) {
                        attribute(&mut self.world, a, &m, pen);
                    }
                }
            }
            // A dimension's lines, arrows and text are in an anonymous block, already in world coordinates.
            EntityType::Dimension(d) => self.expand(&d.base().block_name, m, layer, pen, ctx),
            // Attribute definitions show as attributes on the INSERT instead.
            EntityType::AttributeDefinition(_) => {}
            other => w.skip(other.as_entity().entity_type()),
        }
    }

    /// Effective layer name and pen, or None if the entity is invisible.
    fn style(&mut self, c: &'a EntityCommon, ctx: &Ctx<'a>) -> Option<(&'a str, Pen)> {
        if c.invisible {
            return None;
        }
        let own = if c.layer.is_empty() { "0" } else { c.layer.as_str() };
        let (index, rgb) = color(&c.color);
        Some(dxf::style(&mut self.world, own, rgb, index, ctx))
    }

    /// Draws block `name` with transform `m`, as the child of an INSERT or DIMENSION.
    fn expand(&mut self, name: &str, m: Affine, layer: &'a str, pen: Pen, ctx: &Ctx<'a>) {
        if ctx.depth >= dxf::MAX_DEPTH {
            return;
        }
        let inner = Ctx { m, layer: Some(layer), color: pen.color, depth: ctx.depth + 1 };
        let doc = self.doc;
        for e in doc.entities_in_block(name) {
            self.entity(e, &inner);
        }
    }
}

#[cfg(test)]
mod tests {
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
    fn rejects_old_and_non_dwg() {
        assert!(decode(b"AC1009\0\0").err().unwrap().contains("R13"));
        assert!(decode(b"hello").is_err());
    }
}
