//! Copies a DXF or DWG and adds a labelled grid of elements the baseline sample lacks, for
//! side-by-side comparison with AutoCAD:
//! `cargo run --release --example extend_baseline -- samples/baseline-sample.dxf samples/baseline-extended.dxf`.
//! The output keeps the input's format (by extension) and version. The grid sits below the
//! drawing (negative Y); each case is on its own `X_<NAME>` layer so it can be isolated. RAY and
//! XLINE get a strip under the grid so they cross nothing else. Dimensions and tables are written
//! without their anonymous blocks; AutoCAD regenerates those on open, a viewer may not.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};
use std::path::Path;

use acadrust::entities::acis::primitives;
use acadrust::entities::*;
use acadrust::objects::ObjectType;
use acadrust::tables::Layer;
use acadrust::types::{Color, Handle, LineWeight, Transparency, Vector2, Vector3};
use acadrust::{BlockRecord, CadDocument, DwgReader, DwgWriter, DxfReader, DxfWriter};

const COLUMNS: usize = 8;
const WIDTH: f64 = 150.;
const HEIGHT: f64 = 110.;

fn v(x: f64, y: f64) -> Vector3 {
    Vector3::new(x, y, 0.)
}

/// Cell-local point helper: `o.p(x, y)` is (x, y) inside the 140 × 94 frame at `o`.
#[derive(Clone, Copy)]
struct Cell(Vector3);

impl Cell {
    fn p(self, x: f64, y: f64) -> Vector3 {
        self.0 + v(x, y)
    }
    fn p3(self, x: f64, y: f64, z: f64) -> Vector3 {
        self.0 + Vector3::new(x, y, z)
    }
    fn q(self, x: f64, y: f64) -> Vector2 {
        Vector2::new(self.0.x + x, self.0.y + y)
    }
}

struct Sheet {
    doc: CadDocument,
    top_left: Vector3,
    count: usize,
}

impl Sheet {
    fn layer(&mut self, name: &str, color: u8) {
        if self.doc.layers.get(name).is_none() {
            let mut l = Layer::new(name);
            l.handle = self.doc.allocate_handle();
            l.color = Color::Index(color);
            self.doc.layers.add(l).unwrap();
        }
    }

    fn put(&mut self, layer: &str, mut e: EntityType) {
        e.common_mut().layer = layer.into();
        self.doc.add_entity(e).unwrap();
    }

    fn label(&mut self, text: &str, at: Vector3, height: f64) {
        self.put("X_LABELS", EntityType::Text(Text::with_value(text, at).with_height(height)));
    }

    /// Next grid cell: frame, label and layer; returns the cell and its layer name.
    fn next(&mut self, name: &str) -> (Cell, String) {
        let i = self.count;
        self.count += 1;
        let o = Cell(self.top_left + v((i % COLUMNS) as f64 * WIDTH, -((i / COLUMNS + 1) as f64) * HEIGHT));
        self.frame(o, &format!("{:02} {name}", i + 1));
        let layer = format!("X_{}", name.replace([' ', '/', ',', '(', ')', '+'], "_"));
        self.layer(&layer, (i % 6 + 1) as u8);
        (o, layer)
    }

    fn frame(&mut self, o: Cell, title: &str) {
        let corners = [o.q(0., 0.), o.q(140., 0.), o.q(140., 94.), o.q(0., 94.)];
        let mut f = LwPolyline::from_points(corners.to_vec());
        f.close();
        self.put("X_FRAMES", EntityType::LwPolyline(f));
        self.label(title, o.p(4., 87.), 3.);
    }

    fn add(&mut self, name: &str, build: impl FnOnce(&mut CadDocument, Cell) -> Vec<EntityType>) {
        let (o, layer) = self.next(name);
        for e in build(&mut self.doc, o) {
            self.put(&layer, e);
        }
    }

    /// Defines a block from entities authored around the block origin.
    fn block(&mut self, name: &str, entities: Vec<EntityType>) -> Handle {
        let mut b = BlockRecord::new(name);
        b.flags.anonymous = name.starts_with('*');
        b.handle = self.doc.allocate_handle();
        b.block_entity_handle = self.doc.allocate_handle();
        b.block_end_handle = self.doc.allocate_handle();
        let owner = b.handle;
        self.doc.block_records.add(b).unwrap();
        for mut e in entities {
            e.common_mut().owner_handle = owner;
            self.doc.add_entity(e).unwrap();
        }
        owner
    }
}

fn line(a: Vector3, b: Vector3) -> EntityType {
    EntityType::Line(Line::from_points(a, b))
}

fn text(s: &str, at: Vector3, h: f64) -> EntityType {
    EntityType::Text(Text::with_value(s, at).with_height(h))
}

/// Width of `s` in Arial (the Standard style's font) at text height `h`, from Arial's advance widths
/// per 1000 em and its cap height of 716.
fn arial_width(s: &str, h: f64) -> f64 {
    const LOWER: [u16; 26] = [556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500];
    const UPPER: [u16; 26] = [667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611];
    let em: u32 = s.bytes().map(|b| match b {
        b'a'..=b'z' => LOWER[(b - b'a') as usize] as u32,
        b'A'..=b'Z' => UPPER[(b - b'A') as usize] as u32,
        b'0'..=b'9' => 556,
        _ => 278,
    }).sum();
    em as f64 / 716. * h
}

/// A justified TEXT at `at`. AutoCAD draws a TEXT from its insertion point as stored, so that
/// point is set to where the left end of the baseline lands (descent taken as 0.3·height).
fn justified(s: &str, at: Vector3, h: f64, ha: TextHorizontalAlignment, va: TextVerticalAlignment) -> Text {
    use TextHorizontalAlignment as H;
    use TextVerticalAlignment as V;
    let dx = match ha { H::Center | H::Middle => 0.5, H::Right => 1., _ => 0. } * arial_width(s, h);
    let dy = match (ha, va) { (H::Middle, _) | (_, V::Middle) => 0.5 * h, (_, V::Top) => h, (_, V::Bottom) => -0.3 * h, _ => 0. };
    let mut t = Text::with_value(s.to_string(), at - v(dx, dy)).with_height(h);
    t.horizontal_alignment = ha;
    t.vertical_alignment = va;
    t.alignment_point = Some(at);
    t
}

fn colored(mut e: EntityType, c: Color) -> EntityType {
    e.common_mut().color = c;
    e
}

/// `text` is the dimension text's middle point; acadrust leaves it at the world origin.
fn dim(mut d: Dimension, text: Vector3) -> EntityType {
    d.base_mut().style_name = "ISO-25".into();
    d.base_mut().text_middle_point = text;
    EntityType::Dimension(d)
}

fn polyline_edge(points: Vec<Vector2>) -> BoundaryPath {
    let mut path = BoundaryPath::new();
    path.add_edge(BoundaryEdge::Polyline(PolylineEdge::new(points, true)));
    path
}

fn rect(o: Cell, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Vector2> {
    vec![o.q(x0, y0), o.q(x1, y0), o.q(x1, y1), o.q(x0, y1)]
}

/// An asymmetric "F" so rotation and mirroring of inserts are visible.
fn f_shape(c: Color) -> Vec<EntityType> {
    let pts = [(0., 0.), (0., 20.), (12., 20.), (12., 16.), (4., 16.), (4., 12.), (10., 12.), (10., 8.), (4., 8.), (4., 0.)];
    let mut f = LwPolyline::from_points(pts.iter().map(|&(x, y)| Vector2::new(x, y)).collect());
    f.close();
    vec![colored(EntityType::LwPolyline(f), c), colored(EntityType::Circle(Circle::from_coords(0., 0., 0., 1.5)), c)]
}

fn build(s: &mut Sheet) {
    for (name, color) in [("X_LABELS", 7), ("X_FRAMES", 8)] {
        s.layer(name, color);
    }
    s.label("EXTRA ELEMENTS (synthetic, acadrust)", s.top_left + v(0., 15.), 12.);

    // Blocks used by the insert cells.
    s.block("X_F", f_shape(Color::ByBlock));
    let mut tag = AttributeDefinition::new("TAG".into(), "Tag?".into(), "DEF".into());
    tag.insertion_point = v(-6., -12.);
    tag.height = 4.;
    let attdef_handle = s.doc.allocate_handle();
    tag.common.handle = attdef_handle;
    s.block("X_TAG", vec![EntityType::Circle(Circle::from_coords(0., 0., 0., 8.)), EntityType::AttributeDefinition(tag)]);
    let mut nested = f_shape(Color::Index(3));
    nested.push(EntityType::Insert(Insert::new("X_F", v(25., 0.)).with_rotation(FRAC_PI_4)));
    nested.push(EntityType::Circle(Circle::from_coords(12., 10., 0., 22.)));
    s.block("X_OUTER", nested);

    // Curves and polylines.
    s.add("SOLID", |_, o| vec![EntityType::Solid(Solid::new(o.p(15., 15.), o.p(60., 15.), o.p(15., 70.), o.p(55., 60.))), EntityType::Solid(Solid::new(o.p(75., 15.), o.p(125., 15.), o.p(125., 70.), o.p(75., 70.)))]);
    s.add("POLYLINE 2D widths", |_, o| {
        let mut pl = Polyline2D::new();
        for (x, y, b, w0, w1) in [(15., 15., 0., 0., 6.), (70., 15., 0.6, 6., 2.), (110., 45., 0., 2., 2.), (70., 75., -0.4, 4., 0.), (20., 60., 0., 0., 0.)] {
            pl.add_vertex(Vertex2D::new(o.p(x, y)).with_bulge(b).with_width(w0, w1));
        }
        pl.close();
        vec![EntityType::Polyline2D(pl)]
    });
    s.add("POLYLINE 3D", |_, o| vec![EntityType::Polyline3D(Polyline3D::from_points(vec![o.p(15., 15.), o.p3(60., 30., 40.), o.p(90., 70.), o.p3(125., 20., -30.)]))]);
    s.add("ELLIPSE arcs", |_, o| {
        let mut a = Ellipse::from_center_axes(o.p(45., 45.), v(30., 0.), 0.5);
        a.start_parameter = 0.3;
        a.end_parameter = 4.5;
        let mut b = Ellipse::from_center_axes(o.p(100., 45.), v(18., 18.), 0.4);
        b.start_parameter = PI;
        b.end_parameter = TAU;
        vec![EntityType::Ellipse(a), EntityType::Ellipse(b)]
    });
    s.add("ARC/CIRCLE normal -Z", |_, o| {
        // OCS x axis for normal -Z is world -X, so OCS x = -world x.
        let mut a = Arc::from_coords(-o.p(40., 40.).x, o.p(40., 40.).y, 0., 25., 0., FRAC_PI_2);
        a.normal = Vector3::new(0., 0., -1.);
        let mut c = Circle::from_coords(-o.p(100., 40.).x, o.p(100., 40.).y, 0., 15.);
        c.normal = Vector3::new(0., 0., -1.);
        vec![EntityType::Arc(a), EntityType::Circle(c), text("arc ends at 9 and 12 o'clock", o.p(10., 8.), 3.)]
    });
    s.add("SPLINE fit points", |_, o| {
        let pts = [(10., 20.), (40., 70.), (70., 30.), (100., 75.), (130., 20.)];
        let fit: Vec<_> = pts.iter().map(|&(x, y)| o.p(x, y)).collect();
        let mut out: Vec<_> = fit.iter().map(|&c| EntityType::Circle(Circle::from_coords(c.x, c.y, 0., 1.))).collect();
        out.push(EntityType::Spline(Spline::from_fit_points(fit)));
        out
    });
    s.add("SPLINE rational", |_, o| {
        // Quarter circle as a degree-2 rational spline; the arc behind it must coincide.
        let mut sp = Spline::from_control_points(2, vec![o.p(110., 15.), o.p(110., 75.), o.p(50., 75.)]);
        sp.weights = vec![1., FRAC_PI_4.cos(), 1.];
        sp.flags.rational = true;
        let c = o.p(50., 15.);
        vec![colored(EntityType::Arc(Arc::from_coords(c.x, c.y, 0., 60., 0., FRAC_PI_2)), Color::Index(8)), EntityType::Spline(sp)]
    });
    s.add("HELIX", |_, o| {
        let mut h = Helix::new();
        h.axis_base_point = o.p(70., 45.);
        h.start_point = o.p(100., 45.);
        h.radius = 30.;
        h.turns = 3.;
        h.turn_height = 5.;
        let c = o.p(70., 45.);
        let points = (0..=144)
            .map(|i| {
                let t = i as f64 / 144. * 3. * TAU;
                Vector3::new(c.x + 30. * t.cos(), c.y + 30. * t.sin(), t / TAU * 5.)
            })
            .collect();
        // Dense cubic control polygon; AutoCAD stores helices as cubic splines.
        h.spline = Spline::from_control_points(3, points);
        vec![EntityType::Helix(h)]
    });

    // Text.
    s.add("TEXT justifications", |_, o| {
        use TextHorizontalAlignment as H;
        use TextVerticalAlignment as V;
        let mut out = vec![];
        for (row, va) in [V::Top, V::Middle, V::Baseline, V::Bottom].into_iter().enumerate() {
            for (col, ha) in [H::Left, H::Center, H::Right].into_iter().enumerate() {
                let at = o.p(25. + col as f64 * 45., 72. - row as f64 * 18.);
                out.push(EntityType::Text(justified(&format!("{ha:?}{va:?}"), at, 3.5, ha, va)));
                out.push(colored(line(at - v(2., 0.), at + v(2., 0.)), Color::Index(1)));
                out.push(colored(line(at - v(0., 2.), at + v(0., 2.)), Color::Index(1)));
            }
        }
        out
    });
    s.add("TEXT fit/aligned/middle", |_, o| {
        let mut out = vec![];
        for (i, ha) in [TextHorizontalAlignment::Aligned, TextHorizontalAlignment::Fit, TextHorizontalAlignment::Middle].into_iter().enumerate() {
            let y = 70. - i as f64 * 25.;
            let label = format!("{ha:?} 10..130");
            let t = if ha == TextHorizontalAlignment::Middle {
                justified(&label, o.p(70., y), 5., ha, TextVerticalAlignment::Baseline)
            } else {
                let mut t = Text::with_value(label, o.p(10., y)).with_height(5.);
                t.horizontal_alignment = ha;
                t.alignment_point = Some(o.p(130., y));
                t
            };
            out.push(EntityType::Text(t));
            out.push(colored(line(o.p(10., y - 3.), o.p(130., y - 3.)), Color::Index(8)));
        }
        out
    });
    s.add("TEXT rot/width/oblique/flags", |_, o| {
        let mut r = Text::with_value("Rotated 30", o.p(10., 10.)).with_height(5.);
        r.rotation = PI / 6.;
        let mut w = Text::with_value("Width 0.5", o.p(70., 70.)).with_height(5.);
        w.width_factor = 0.5;
        let mut q = Text::with_value("Oblique 20", o.p(70., 55.)).with_height(5.);
        q.oblique_angle = 20f64.to_radians();
        let mut b = Text::with_value("Backward", o.p(120., 40.)).with_height(5.);
        b.generation_flags = 2;
        let mut u = Text::with_value("Upside down", o.p(125., 25.)).with_height(5.);
        u.generation_flags = 4;
        let c = Text::with_value("%%c25 %%d %%p0.1 %%uunder%%u %%oover%%o", o.p(5., 80.)).with_height(3.5);
        [r, w, q, b, u, c].into_iter().map(EntityType::Text).collect()
    });
    s.add("MTEXT inline codes", |_, o| {
        let mut m = MText::with_value(
            "\\LUnderline\\l \\OOver\\o \\KStrike\\k {\\H2x;Big} {\\C1;red} {\\c16744448;rgb}\\P\
             Stack 1\\S1/2; 3\\S3#4; x\\Sa^b;\\P\
             {\\fArial|b1|i0;Arial bold} {\\fArial|b0|i1;italic} {\\Q15;oblique}\\P\
             {\\T2;tracked} {\\W2;wide} {\\W0.5;narrow} \\U+00B0 \\U+2300",
            o.p(5., 80.),
        );
        m.height = 4.;
        m.rectangle_width = 130.;
        vec![EntityType::MText(m)]
    });
    s.add("MTEXT attach/rot/mask", |_, o| {
        let mut out = vec![];
        for (i, (x, y)) in [(25., 70.), (70., 70.), (115., 70.)].into_iter().enumerate() {
            let at = o.p(x, y);
            let mut m = MText::with_value("Attach\\Ppoint", at);
            m.height = 4.;
            m.attachment_point = [AttachmentPoint::TopLeft, AttachmentPoint::MiddleCenter, AttachmentPoint::BottomRight][i];
            out.push(EntityType::MText(m));
            out.push(colored(EntityType::Circle(Circle::from_coords(at.x, at.y, 0., 1.)), Color::Index(1)));
        }
        out.push(colored(line(o.p(10., 30.), o.p(130., 30.)), Color::Index(3)));
        out.push(colored(line(o.p(10., 20.), o.p(130., 20.)), Color::Index(3)));
        let mut mask = MText::with_value("Background mask", o.p(70., 25.));
        mask.height = 5.;
        mask.attachment_point = AttachmentPoint::MiddleCenter;
        mask.background_fill_flags = 1;
        mask.background_scale = 1.5;
        mask.background_color = Color::Index(2);
        out.push(EntityType::MText(mask));
        let mut rot = MText::with_value("Rotated -20", o.p(10., 50.));
        rot.height = 4.;
        rot.rotation = -20f64.to_radians();
        out.push(EntityType::MText(rot));
        out
    });

    // Blocks.
    s.add("ATTDEF in model space", |_, o| {
        let mut a = AttributeDefinition::new("PART_NO".into(), "Part number".into(), "P-0001".into());
        a.insertion_point = o.p(20., 45.);
        a.height = 6.;
        vec![EntityType::AttributeDefinition(a)]
    });
    let (o, layer) = s.next("INSERT + ATTRIB");
    let mut ins = Insert::new("X_TAG", o.p(70., 50.));
    let mut attr = AttributeEntity::new("TAG".into(), "A-17".into());
    attr.insertion_point = o.p(64., 38.);
    attr.alignment_point = attr.insertion_point;
    attr.height = 4.;
    attr.attdef_handle = attdef_handle;
    attr.common.layer = layer.clone();
    ins.attributes.push(attr);
    s.put(&layer, EntityType::Insert(ins));
    s.add("MINSERT 3x2", |_, o| vec![EntityType::Insert(Insert::new("X_F", o.p(20., 15.)).with_array(3, 2, 40., 40.))]);
    s.add("INSERT rot/scale/mirror", |_, o| {
        vec![
            EntityType::Insert(Insert::new("X_F", o.p(15., 15.))),
            EntityType::Insert(Insert::new("X_F", o.p(55., 15.)).with_rotation(PI / 6.)),
            EntityType::Insert(Insert::new("X_F", o.p(80., 15.)).with_scale(2., 1., 1.)),
            EntityType::Insert(Insert::new("X_F", o.p(40., 55.)).with_scale(-1., 1., 1.)),
            EntityType::Insert(Insert::new("X_F", o.p(80., 75.)).with_scale(1., -1.5, 1.)),
        ]
    });
    s.add("NESTED INSERT ByBlock", |_, o| {
        let mut red = Insert::new("X_OUTER", o.p(30., 30.));
        red.common.color = Color::Index(1);
        vec![EntityType::Insert(red), text("F: ByBlock -> red; outer F green", o.p(5., 5.), 3.)]
    });

    // Dimensions (no anonymous blocks; see the module doc).
    s.add("DIM linear", |_, o| {
        let mut h = DimensionLinear::horizontal(o.p(15., 20.), o.p(70., 40.));
        h.definition_point = o.p(15., 60.);
        let mut r = DimensionLinear::vertical(o.p(95., 15.), o.p(115., 70.));
        r.definition_point = o.p(130., 15.);
        vec![line(o.p(15., 20.), o.p(70., 40.)), line(o.p(95., 15.), o.p(115., 70.)), dim(Dimension::Linear(h), o.p(42.5, 62.)), dim(Dimension::Linear(r), o.p(128., 42.5))]
    });
    s.add("DIM rotated + override", |_, o| {
        let mut r = DimensionLinear::rotated(o.p(15., 20.), o.p(110., 50.), PI / 6.);
        r.definition_point = o.p(10., 50.);
        r.base.user_text = Some("<> TYP".into());
        // Midpoint of the dimension line: the line through (10, 50) at 30°, at the definition points' middle.
        let d = v((PI / 6.).cos(), (PI / 6.).sin());
        let t = (o.p(62.5, 35.) - o.p(10., 50.)).dot(&d);
        vec![line(o.p(15., 20.), o.p(110., 50.)), dim(Dimension::Linear(r), o.p(10., 50.) + d * t + v(-1., 2.))]
    });
    s.add("DIM aligned", |_, o| {
        let mut d = DimensionAligned::new(o.p(20., 20.), o.p(110., 60.));
        d.set_offset(15.);
        let text = d.definition_point + (d.first_point - d.second_point) * 0.5;
        vec![line(o.p(20., 20.), o.p(110., 60.)), dim(Dimension::Aligned(d), text)]
    });
    s.add("DIM radius + diameter", |_, o| {
        let (a, b) = (o.p(35., 45.), o.p(100., 45.));
        vec![
            EntityType::Circle(Circle::from_coords(a.x, a.y, 0., 25.)),
            EntityType::Circle(Circle::from_coords(b.x, b.y, 0., 25.)),
            dim(Dimension::Radius(DimensionRadius::new(a, a + v(25. * FRAC_PI_4.cos(), 25. * FRAC_PI_4.sin()))), a + v(32., 32.)),
            dim(Dimension::Diameter(DimensionDiameter::new(b + v(-25. * 0.6, -25. * 0.8), b + v(25. * 0.6, 25. * 0.8))), b + v(3., 4.)),
        ]
    });
    s.add("DIM angular 2-line", |_, o| {
        let (c, a, b) = (o.p(20., 15.), o.p(120., 15.), o.p(90., 75.));
        let mut d = DimensionAngular2Ln::new(c, a, b);
        d.dimension_arc = o.p(80., 50.);
        let r = (d.dimension_arc - c).length();
        let mid = ((a - c).normalize() + (b - c).normalize()).normalize();
        vec![line(c, a), line(c, b), dim(Dimension::Angular2Ln(d), c + mid * (r + 3.))]
    });
    s.add("DIM angular 3-point", |_, o| {
        let (c, a, b) = (o.p(70., 20.), o.p(120., 30.), o.p(25., 60.));
        let mut d = DimensionAngular3Pt::new(c, a, b);
        d.definition_point = o.p(70., 70.);
        let mid = ((a - c).normalize() + (b - c).normalize()).normalize();
        vec![EntityType::Point(Point::from_coords(c.x, c.y, 0.)), line(c, a), line(c, b), dim(Dimension::Angular3Pt(d), c + mid * 53.)]
    });
    s.add("DIM ordinate X/Y", |_, o| {
        let mut out = vec![EntityType::LwPolyline({
            let mut r = LwPolyline::from_points(rect(o, 20., 15., 90., 60.));
            r.close();
            r
        })];
        // Ordinates measured from the rectangle's lower-left corner.
        for (x_type, end, text) in [(true, o.p(90., 80.), o.p(90., 84.)), (false, o.p(120., 60.), o.p(128., 60.))] {
            let mut d = DimensionOrdinate::new(o.p(90., 60.), end, x_type);
            d.definition_point = o.p(20., 15.);
            d.refresh_measurement();
            out.push(dim(Dimension::Ordinate(d), text));
        }
        out
    });
    s.add("DIM arc length", |_, o| {
        let c = o.p(40., 20.);
        let d = DimensionArc {
            center_point: c,
            first_extension_point: o.p(100., 20.),
            second_extension_point: o.p(40., 80.),
            definition_point: o.p(95., 70.),
            arc_end_parameter: FRAC_PI_2,
            ..Default::default()
        };
        vec![EntityType::Arc(Arc::from_coords(c.x, c.y, 0., 60., 0., FRAC_PI_2)), dim(Dimension::Arc(d), c + v(FRAC_PI_4.cos(), FRAC_PI_4.sin()) * 76.)]
    });
    s.add("DIM jogged radius", |_, o| {
        let c = o.p(20., 15.);
        let d = DimensionLargeRadial {
            definition_point: c,
            chord_point: c + v(100. * 0.8, 100. * 0.6) * 0.7,
            override_center: o.p(40., 25.),
            jog_point: o.p(60., 45.),
            jog_angle: FRAC_PI_4,
            ..Default::default()
        };
        vec![EntityType::Arc(Arc::from_coords(c.x, c.y, 0., 70., 0.2, 1.2)), dim(Dimension::LargeRadial(d), o.p(42., 30.))]
    });

    // Annotation.
    s.add("LEADER straight + spline", |_, o| {
        let mut a = Leader::from_vertices(vec![o.p(15., 15.), o.p(45., 55.), o.p(65., 55.)]);
        a.creation_type = LeaderCreationType::NoAnnotation;
        let mut b = Leader::from_vertices(vec![o.p(80., 15.), o.p(95., 45.), o.p(110., 35.), o.p(125., 70.)]);
        b.creation_type = LeaderCreationType::NoAnnotation;
        b.path_type = LeaderPathType::Spline;
        vec![EntityType::Leader(a), EntityType::Leader(b), text("Note", o.p(67., 54.), 3.5)]
    });
    s.add("TOLERANCE (GD&T)", |_, o| {
        let mut t = Tolerance::with_text(o.p(10., 60.), "{\\Fgdt;j}%%v{\\Fgdt;n}0.05{\\Fgdt;m}%%vA%%vB%%vC");
        t.dimension_style_name = "ISO-25".into();
        let mut u = Tolerance::with_text(o.p(10., 30.), "{\\Fgdt;u}%%v0.1%%v^J{\\Fgdt;r}%%v0.02%%vA");
        u.dimension_style_name = "ISO-25".into();
        vec![EntityType::Tolerance(t), EntityType::Tolerance(u)]
    });
    s.add("MLINE", |doc, o| {
        let mut m = MLine::from_points(&[o.p(15., 15.), o.p(60., 15.), o.p(90., 60.), o.p(125., 60.)]);
        m.style_handle = doc.objects.iter().find_map(|(h, ob)| matches!(ob, ObjectType::MLineStyle(_)).then_some(*h));
        m.scale_factor = 8.;
        // Element parameters: distance along the miter to each STANDARD element (offsets ±0.5).
        for vx in &mut m.vertices {
            let across = Vector3::UNIT_Z.cross(&vx.direction).dot(&vx.miter);
            for (seg, offset) in vx.segments.iter_mut().zip([0.5, -0.5]) {
                seg.parameters = vec![offset * 8. / across, 0.];
                seg.area_fill_parameters.clear();
            }
        }
        vec![EntityType::MLine(m)]
    });
    // TABLE with its anonymous *T block filled in (grid lines and cell MTEXT, relative to the
    // insertion point at the top-left corner); acadrust writes the block empty otherwise.
    let (o, layer) = s.next("TABLE");
    let (rows, cols, w, h) = (3usize, 3usize, 40., 15.);
    let mut graphics = vec![];
    for i in 0..=rows {
        graphics.push(line(v(0., -(i as f64) * h), v(cols as f64 * w, -(i as f64) * h)));
    }
    for j in 0..=cols {
        graphics.push(line(v(j as f64 * w, 0.), v(j as f64 * w, -(rows as f64) * h)));
    }
    let label = |i: usize, j: usize| if i == 0 { format!("Col {j}") } else { format!("{}", i * 10 + j) };
    for i in 0..rows {
        for j in 0..cols {
            let mut m = MText::with_value(label(i, j), v((j as f64 + 0.5) * w, -(i as f64 + 0.5) * h));
            m.height = 4.;
            m.rectangle_width = 0.;
            m.attachment_point = AttachmentPoint::MiddleCenter;
            graphics.push(EntityType::MText(m));
        }
    }
    let block = s.block("*T1", graphics);
    let mut t = Table::new(o.p(10., 80.), rows, cols);
    t.table_style_handle = s.doc.objects.iter().find_map(|(h, ob)| matches!(ob, ObjectType::TableStyle(_)).then_some(*h));
    t.block_name = "*T1".into();
    t.block_record_handle = Some(block);
    for c in &mut t.columns {
        c.width = w;
    }
    for (i, row) in t.rows.iter_mut().enumerate() {
        row.height = h;
        for (j, cell) in row.cells.iter_mut().enumerate() {
            *cell = TableCell::text(&label(i, j));
        }
    }
    s.put(&layer, EntityType::Table(t));
    s.add("WIPEOUT", |_, o| {
        let mut out: Vec<_> = (0..8).map(|i| line(o.p(10. + i as f64 * 16., 10.), o.p(10. + i as f64 * 16., 80.))).collect();
        out.push(EntityType::Wipeout(Wipeout::rectangular(o.p(35., 30.), 70., 30.)));
        out.push(text("masked", o.p(55., 43.), 4.));
        out
    });

    // Hatches and fills.
    s.add("HATCH gradient", |_, o| {
        let mut out = vec![];
        for (i, (name, angle)) in [("LINEAR", 0.), ("CYLINDER", FRAC_PI_2), ("SPHERICAL", 0.)].into_iter().enumerate() {
            let x = 8. + i as f64 * 44.;
            let mut h = Hatch::solid();
            h.add_path(polyline_edge(rect(o, x, 15., x + 38., 75.)));
            h.gradient_color.enabled = true;
            h.gradient_color.name = name.into();
            h.gradient_color.angle = angle;
            h.gradient_color.add_color(0., Color::Index(5));
            h.gradient_color.add_color(1., Color::Index(2));
            out.push(EntityType::Hatch(h));
            out.push(text(name, o.p(x, 8.), 3.));
        }
        out
    });
    s.add("HATCH island, curved edges", |_, o| {
        let mut h = Hatch::new();
        h.is_solid = false;
        h.pattern = HatchPattern::new("ANSI31");
        h.pattern.add_line(HatchPatternLine { angle: FRAC_PI_4, base_point: Vector2::new(0., 0.), offset: Vector2::new(-3.175, 3.175), dash_lengths: vec![] });
        h.add_path(polyline_edge(rect(o, 10., 10., 130., 80.)));
        let mut circle = BoundaryPath::new();
        circle.add_edge(BoundaryEdge::CircularArc(CircularArcEdge { center: o.q(40., 45.), radius: 20., start_angle: 0., end_angle: TAU, counter_clockwise: true }));
        h.add_path(circle);
        let mut ell = BoundaryPath::new();
        ell.add_edge(BoundaryEdge::EllipticArc(EllipticArcEdge { center: o.q(98., 45.), major_axis_endpoint: Vector2::new(22., 0.), minor_axis_ratio: 0.5, start_angle: 0., end_angle: TAU, counter_clockwise: true }));
        h.add_path(ell);
        vec![EntityType::Hatch(h)]
    });
    s.add("TRANSPARENCY", |_, o| {
        let mut out = vec![];
        for (i, (pct, c)) in [(0., 1), (50., 3), (80., 5)].into_iter().enumerate() {
            let x = 15. + i as f64 * 30.;
            let mut h = Hatch::solid();
            h.add_path(polyline_edge(rect(o, x, 15. + i as f64 * 10., x + 55., 60. + i as f64 * 10.)));
            h.common.color = Color::Index(c);
            h.common.transparency = Transparency::from_percent(pct);
            out.push(EntityType::Hatch(h));
            out.push(text(&format!("{pct}%"), o.p(x, 8.), 3.));
        }
        out
    });

    // Properties.
    s.add("TRUE COLOR + LINEWEIGHT", |_, o| {
        let mut out = vec![];
        for (i, (w, (r, g, b))) in [(0, (255, 0, 0)), (25, (255, 128, 0)), (50, (0, 160, 0)), (100, (0, 128, 255)), (211, (160, 0, 200))].into_iter().enumerate() {
            let y = 75. - i as f64 * 14.;
            let mut l = Line::from_points(o.p(40., y), o.p(130., y));
            l.common.color = Color::Rgb { r, g, b };
            l.common.line_weight = LineWeight::from_value(w);
            out.push(EntityType::Line(l));
            out.push(text(&format!("{:.2} mm", w as f64 / 100.), o.p(5., y - 1.5), 3.));
        }
        out
    });
    s.add("LINETYPE scale", |doc, o| {
        let mut out = vec![];
        for (i, (lt, scale)) in [("ACAD_ISO02W100", 0.25), ("ACAD_ISO02W100", 1.), ("ACAD_ISO04W100", 1.), ("ACAD_ISO07W100", 2.)].into_iter().enumerate() {
            let y = 75. - i as f64 * 16.;
            let mut l = Line::from_points(o.p(40., y), o.p(130., y));
            l.common.linetype = lt.into();
            l.common.linetype_handle = doc.line_types.get(lt).map(|t| t.handle);
            l.common.linetype_scale = scale;
            out.push(EntityType::Line(l));
            out.push(text(&format!("{} x{scale}", &lt[5..10]), o.p(5., y - 1.5), 3.));
        }
        out
    });

    // Meshes and solids.
    s.add("POLYFACE MESH", |_, o| {
        let mut pf = PolyfaceMesh::new();
        let p = [o.p3(30., 20., 0.), o.p3(100., 20., 0.), o.p3(100., 70., 0.), o.p3(30., 70., 0.), o.p3(65., 45., 40.)];
        let ids: Vec<_> = p.iter().map(|q| pf.add_vertex_xyz(q.x, q.y, q.z)).collect();
        for k in 0..4 {
            pf.add_triangle(ids[k], ids[(k + 1) % 4], ids[4]);
        }
        vec![EntityType::PolyfaceMesh(pf)]
    });
    s.add("POLYGON MESH 4x4", |_, o| {
        let mut pm = PolygonMeshEntity::new();
        pm.m_vertex_count = 4;
        pm.n_vertex_count = 4;
        pm.vertices = (0..16)
            .map(|k| {
                let (i, j) = ((k / 4) as f64, (k % 4) as f64);
                PolygonMeshVertex::at(o.p3(20. + i * 33., 15. + j * 22., 10. * (i + j).sin()))
            })
            .collect();
        vec![EntityType::PolygonMesh(pm)]
    });
    s.add("MESH", |_, o| {
        let p = vec![o.p(20., 15.), o.p(120., 15.), o.p(120., 75.), o.p(20., 75.), o.p3(70., 45., 30.)];
        vec![EntityType::Mesh(Mesh::from_triangles(p, &[(0, 1, 4), (1, 2, 4), (2, 3, 4), (3, 0, 4)]))]
    });
    s.add("3DSOLID primitives", |_, o| {
        let c = |x: f64, y: f64, z: f64| [o.0.x + x, o.0.y + y, z];
        [
            primitives::build_box(c(25., 45., 10.), 30., 30., 20.),
            primitives::build_cylinder(c(70., 45., 0.), 15., 30.),
            primitives::build_sphere(c(112., 45., 15.), 15.),
        ]
        .into_iter()
        .map(|sat| EntityType::Solid3D(Solid3D::from_sat(&sat.to_sat_string())))
        .collect()
    });
    s.add("BODY / torus", |_, o| {
        let torus = primitives::build_torus([o.0.x + 70., o.0.y + 45., 0.], 30., 8.);
        vec![EntityType::Solid3D(Solid3D::from_sat(&torus.to_sat_string()))]
    });

    // Infinite lines in their own strip below the grid.
    let rows = (s.count + COLUMNS - 1) / COLUMNS;
    let y = s.top_left.y - rows as f64 * HEIGHT - 40.;
    let x = s.top_left.x;
    for (name, color) in [("X_RAY", 1), ("X_XLINE", 3)] {
        s.layer(name, color);
    }
    s.label("RAY from the dot, to the right", v(x, y + 4.), 4.);
    s.put("X_RAY", EntityType::Point(Point::from_coords(x + 200., y, 0.)));
    s.put("X_RAY", EntityType::Ray(Ray::new(v(x + 200., y), Vector3::UNIT_X)));
    s.label("XLINE through the dot, both ways", v(x, y - 36.), 4.);
    s.put("X_XLINE", EntityType::Point(Point::from_coords(x + 200., y - 40., 0.)));
    s.put("X_XLINE", EntityType::XLine(XLine::new(v(x + 200., y - 40.), Vector3::UNIT_X)));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output] = &args[..] else {
        panic!("usage: extend_baseline IN.(dxf|dwg) OUT.(dxf|dwg)");
    };
    let dwg = |p: &str| Path::new(p).extension().is_some_and(|e| e.eq_ignore_ascii_case("dwg"));
    let doc = if dwg(input) { DwgReader::from_file(input).unwrap().read().unwrap() } else { DxfReader::from_file(input).unwrap().read().unwrap() };
    let before = doc.entity_count();
    // Below the model-space extents, left-aligned with them.
    let (min, _) = (doc.header.model_space_extents_min, doc.header.model_space_extents_max);
    let mut s = Sheet { top_left: v(min.x, min.y - 100.), doc, count: 0 };
    build(&mut s);
    // Class-based entities (WIPEOUT, HELIX, MESH, ...) need their class, or the DWG writer files
    // them under a fallback type code that AutoCAD drops.
    let missing: Vec<String> = ["WIPEOUT", "HELIX", "MESH", "ACAD_TABLE", "MLINE", "ARC_DIMENSION", "LARGE_RADIAL_DIMENSION", "MULTILEADER"]
        .into_iter()
        .filter(|c| !s.doc.classes.contains(c))
        .map(String::from)
        .collect();
    s.doc.classes.update_defaults();
    println!("classes added: {missing:?}");
    println!("{} cells, {} → {} entities", s.count, before, s.doc.entity_count());
    if dwg(output) {
        DwgWriter::write_to_file(output, &s.doc).unwrap();
    } else {
        std::fs::write(output, DxfWriter::new(&s.doc).write_to_vec().unwrap()).unwrap();
    }
}
