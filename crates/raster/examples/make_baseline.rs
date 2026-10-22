//! Writes the test drawing `baseline.dxf` and `baseline.dwg` from scratch, so it can be committed
//! and decoded in CI (`tests/fixtures.rs`):
//! `cargo run --release --example make_baseline -- crates/raster/tests/fixtures`.
//! It holds the cells below, which cover what the third-party baseline sample did, followed by the
//! grid of `shared/cells.rs`. Everything is authored here; nothing is copied from another drawing.

#[path = "shared/cells.rs"]
mod cells;

use std::f64::consts::PI;

use acadrust::entities::*;
use acadrust::objects::ObjectType;
use acadrust::tables::{DimStyle, Layer, LineType, LineTypeElement};
use acadrust::types::{Color, Transparency, Vector2, Vector3};
use acadrust::{CadDocument, EntityType};
use cells::{build, colored, line, polyline_edge, rect, save, text, v, Sheet};

/// Lines, dashes and dots of the ISO linetypes the grid uses, in mm.
const LINETYPES: [(&str, &str, &[f64]); 3] = [
    ("ACAD_ISO02W100", "ISO dash __ __ __", &[12., -3.]),
    ("ACAD_ISO04W100", "ISO long-dash dot ____ . ____ .", &[24., -3., 0., -3.]),
    ("ACAD_ISO07W100", "ISO dot . . . .", &[0., -3.]),
];

/// The tables a new acadrust document lacks: millimetre units, an Arial Standard style, the ISO
/// linetypes and the ISO-25 dimension style.
fn setup(doc: &mut CadDocument) {
    doc.header.insertion_units = 4;
    doc.header.measurement = 1;
    if let Some(s) = doc.text_styles.get_mut("Standard") {
        s.font_file = "arial.ttf".into();
    }
    for (name, description, pattern) in LINETYPES {
        let mut lt = LineType::new(name);
        lt.description = description.into();
        for &len in pattern {
            lt.add_element(if len < 0. { LineTypeElement::space(len) } else { LineTypeElement::dash(len) });
        }
        lt.handle = doc.allocate_handle();
        doc.line_types.add(lt).unwrap();
    }
    let mut d = DimStyle::new("ISO-25");
    d.handle = doc.allocate_handle();
    d.dimasz = 2.5;
    d.dimtxt = 2.5;
    d.dimexo = 0.625;
    d.dimexe = 1.25;
    d.dimgap = 0.625;
    d.dimtad = 1;
    d.dimdec = 2;
    d.dimzin = 8;
    d.dimdsep = b',' as i16;
    d.dimtih = false;
    d.dimtoh = false;
    d.dimtxsty_handle = doc.header.current_text_style_handle;
    doc.dim_styles.add(d).unwrap();
}

/// ACIS text of a planar REGION bounded by the polygon `pts` (counter-clockwise, Z = 0).
fn region_sat(pts: &[Vector3]) -> String {
    let n = pts.len();
    // Record indices: 0 header, 1 body, 2 lump, 3 shell, 4 face, 5 loop, 6 plane, then per edge
    // a coedge, edge, vertex, curve and point.
    let (coedge, edge, vertex, curve, point) = (|i: usize| 7 + i % n, |i: usize| 7 + n + i % n, |i: usize| 7 + 2 * n + i % n, |i: usize| 7 + 3 * n + i, |i: usize| 7 + 4 * n + i);
    let mut s = String::from("22300 0 1 0\n8 acadrust 20 ASM 230.1.0.65535 NT 24 Wed Oct 22 00:00:00 2026\n1 1e-006 1e-010\n");
    s += "asmheader $-1 -1 @13 230.1.0.65535 #\nbody $-1 -1 $-1 $2 $-1 $-1 #\nlump $-1 -1 $-1 $-1 $3 $1 #\n";
    s += "shell $-1 -1 $-1 $-1 $-1 $4 $-1 $2 #\nface $-1 -1 $-1 $-1 $5 $3 $-1 $6 forward double out #\n";
    s += "loop $-1 -1 $-1 $-1 $7 $4 #\n";
    s += &format!("plane-surface $-1 -1 $-1 {} {} 0 0 0 1 1 0 0 forward_v I I I I #\n", pts[0].x, pts[0].y);
    let len = |i: usize| (pts[(i + 1) % n] - pts[i]).length();
    for i in 0..n {
        s += &format!("coedge $-1 -1 $-1 ${} ${} $-1 ${} forward $5 0 $-1 #\n", coedge(i + 1), coedge(i + n - 1), edge(i));
    }
    for i in 0..n {
        s += &format!("edge $-1 -1 $-1 ${} 0 ${} {} ${} ${} forward @7 unknown #\n", vertex(i), vertex(i + 1), len(i), coedge(i), curve(i));
    }
    for i in 0..n {
        s += &format!("vertex $-1 -1 $-1 ${} 2 ${} #\n", edge(i), point(i));
    }
    for i in 0..n {
        let d = (pts[(i + 1) % n] - pts[i]) * (1. / len(i));
        s += &format!("straight-curve $-1 -1 $-1 {} {} 0 {} {} 0 F 0 F {} #\n", pts[i].x, pts[i].y, d.x, d.y, len(i));
    }
    for p in pts {
        s += &format!("point $-1 -1 $-1 {} {} 0 #\n", p.x, p.y);
    }
    s + "End-of-ACIS-data\n"
}

/// A hatch pattern line from an acad.pat row `angle, x, y, dx, dy [, dashes]` at `scale` and
/// `rotation` (degrees): DXF stores the offset rotated with the line and everything scaled.
fn pat(angle: f64, x: f64, y: f64, dx: f64, dy: f64, dashes: &[f64], scale: f64, rotation: f64) -> HatchPatternLine {
    let a = (angle + rotation).to_radians();
    let r = rotation.to_radians();
    let (dx, dy) = (dx * scale, dy * scale);
    HatchPatternLine {
        angle: a,
        base_point: Vector2::new((x * r.cos() - y * r.sin()) * scale, (x * r.sin() + y * r.cos()) * scale),
        offset: Vector2::new(dx * a.cos() - dy * a.sin(), dx * a.sin() + dy * a.cos()),
        dash_lengths: dashes.iter().map(|d| d * scale).collect(),
    }
}

fn hatch(name: &str, lines: Vec<HatchPatternLine>, boundary: Vec<Vector2>, scale: f64, rotation: f64) -> EntityType {
    let mut h = Hatch::new();
    h.is_solid = false;
    h.pattern_type = HatchPatternType::Predefined;
    h.pattern = HatchPattern::new(name);
    h.pattern_scale = scale;
    h.pattern_angle = rotation.to_radians();
    for l in lines {
        h.pattern.add_line(l);
    }
    h.add_path(polyline_edge(boundary));
    EntityType::Hatch(Box::new(h))
}

fn solid_hatch(boundary: Vec<Vector2>) -> Hatch {
    let mut h = Hatch::solid();
    h.add_path(polyline_edge(boundary));
    h
}

fn layer(s: &mut Sheet, name: &str, edit: impl FnOnce(&mut Layer)) {
    s.layer(name, 7);
    edit(s.doc.layers.get_mut(name).unwrap());
}

fn base_cells(s: &mut Sheet) {
    s.add("LINE ARC CIRCLE POINT 3DFACE", |_, o| {
        let mut pl = LwPolyline::from_points(vec![o.q(10., 10.), o.q(40., 10.), o.q(40., 30.), o.q(10., 30.)]);
        pl.vertices[1].bulge = 0.5;
        pl.close();
        let mut out = vec![line(o.p(10., 80.), o.p(60., 50.)), EntityType::LwPolyline(pl)];
        out.push(EntityType::Arc(Arc::from_coords(o.p(85., 65.).x, o.p(85., 65.).y, 0., 15., 0., PI * 0.75)));
        out.push(EntityType::Circle(Circle::from_coords(o.p(120., 65.).x, o.p(120., 65.).y, 0., 12.)));
        out.extend((0..5).map(|i| EntityType::Point(Point::from_coords(o.p(60. + i as f64 * 6., 40.).x, o.p(0., 40.).y, 0.))));
        out.push(EntityType::Face3D(Face3D::new(o.p(60., 10.), o.p(95., 10.), o.p3(100., 30., 10.), o.p(65., 28.))));
        out.push(EntityType::Face3D(Face3D::triangle(o.p(105., 10.), o.p(135., 10.), o.p(120., 35.))));
        out
    });
    s.add("LWPOLYLINE widths", |_, o| {
        let mut global = LwPolyline::from_points(vec![o.q(10., 75.), o.q(60., 75.), o.q(60., 55.)]);
        global.constant_width = 4.;
        let mut taper = LwPolyline::from_points(vec![o.q(75., 75.), o.q(130., 75.)]);
        taper.vertices[0].start_width = 1.;
        taper.vertices[0].end_width = 10.;
        let mut segments = LwPolyline::from_points(vec![o.q(10., 30.), o.q(45., 15.), o.q(80., 30.), o.q(130., 15.)]);
        for (vx, (w0, w1)) in segments.vertices.iter_mut().zip([(2., 2.), (6., 0.), (0., 4.)]) {
            vx.start_width = w0;
            vx.end_width = w1;
        }
        segments.vertices[1].bulge = 0.4;
        vec![EntityType::LwPolyline(global), EntityType::LwPolyline(taper), EntityType::LwPolyline(segments), text("global 4; taper 1-10; per segment", o.p(10., 42.), 3.)]
    });
    s.add("HATCH patterns", |_, o| {
        const S: f64 = 25.4 / 8.; // acad.pat's metric rows are in units of 1/8 inch.
        let cell = |i: usize| rect(o, 6. + i as f64 * 27., 15., 30. + i as f64 * 27., 80.);
        let ansi31 = |sc: f64, rot: f64| vec![pat(45., 0., 0., 0., S, &[], sc, rot)];
        let w = 4.490128053;
        let mut out = vec![
            hatch("ANSI31", ansi31(1., 0.), cell(0), 1., 0.),
            hatch("ANSI33", vec![pat(45., 0., 0., 0., 2. * S, &[], 1., 0.), pat(45., w, 0., 0., 2. * S, &[S, -S / 2.], 1., 0.)], cell(1), 1., 0.),
            hatch("ANSI37", vec![pat(45., 0., 0., 0., S, &[], 1., 0.), pat(135., 0., 0., 0., S, &[], 1., 0.)], cell(2), 1., 0.),
            hatch("DOTS", vec![pat(0., 0., 0., S / 4., S / 2., &[0., -S / 4.], 1., 0.)], cell(3), 1., 0.),
            hatch("ANSI31", ansi31(2., 30.), cell(4), 2., 30.),
        ];
        for (i, name) in ["ANSI31", "ANSI33", "ANSI37", "DOTS", "x2 30"].into_iter().enumerate() {
            out.push(text(name, o.p(6. + i as f64 * 27., 8.), 3.));
        }
        out
    });

    // Colour and linetype inheritance: blocks whose entity is BYBLOCK, or BYLAYER on layer 0.
    let by_block = |e: EntityType| {
        let mut e = e;
        e.common_mut().color = Color::ByBlock;
        e.common_mut().linetype = "BYBLOCK".into();
        e
    };
    s.block("X_BYBLOCK", vec![by_block(EntityType::Circle(Circle::from_coords(0., 0., 0., 10.)))]);
    s.block("X_LAYER0", vec![EntityType::Circle(Circle::from_coords(0., 0., 0., 10.))]);
    layer(s, "X_RED_DASHED", |l| {
        l.color = Color::Index(1);
        l.line_type = "ACAD_ISO02W100".into();
    });
    let (o, cell_layer) = s.next("COLOR + LINETYPE inheritance");
    s.put("X_RED_DASHED", line(o.p(10., 80.), o.p(60., 80.)));
    s.put(&cell_layer, colored(line(o.p(10., 70.), o.p(60., 70.)), Color::Index(3)));
    s.put(&cell_layer, colored(line(o.p(10., 60.), o.p(60., 60.)), Color::Rgb { r: 0, g: 128, b: 255 }));
    let mut blue = Insert::new("X_BYBLOCK", o.p(25., 30.));
    blue.common.color = Color::Index(5);
    blue.common.linetype = "ACAD_ISO04W100".into();
    s.put(&cell_layer, EntityType::Insert(blue));
    s.put("X_RED_DASHED", EntityType::Insert(Insert::new("X_LAYER0", o.p(60., 30.))));
    for (label, y) in [("layer: red dashed", 80.), ("ACI 3", 70.), ("true colour", 60.)] {
        s.put(&cell_layer, text(label, o.p(65., y - 1.5), 3.));
    }
    s.put(&cell_layer, text("ByBlock: blue, long-dash dot", o.p(5., 12.), 3.));
    s.put(&cell_layer, text("layer 0 in block: red dashed", o.p(75., 12.), 3.));

    // Transparency inherited from the layer and from the insert.
    layer(s, "X_HALF_TRANSPARENT", |l| {
        l.color = Color::Index(3);
        l.transparency = Transparency::from_percent(0.5);
    });
    let mut inner = solid_hatch(vec![Vector2::new(-20., -20.), Vector2::new(20., -20.), Vector2::new(20., 20.), Vector2::new(-20., 20.)]);
    inner.common.color = Color::Index(5);
    inner.common.transparency = Transparency::ByBlock;
    s.block("X_TRANSPARENT_BLOCK", vec![EntityType::Hatch(Box::new(inner))]);
    let (o, cell_layer) = s.next("TRANSPARENCY ByLayer + ByBlock");
    s.put("X_HALF_TRANSPARENT", EntityType::Hatch(Box::new(solid_hatch(rect(o, 10., 25., 60., 80.)))));
    let mut ins = Insert::new("X_TRANSPARENT_BLOCK", o.p(100., 50.));
    ins.common.transparency = Transparency::from_percent(0.8);
    s.put(&cell_layer, EntityType::Insert(ins));
    s.put(&cell_layer, colored(line(o.p(5., 45.), o.p(135., 45.)), Color::Index(1)));
    s.put(&cell_layer, text("layer 50%", o.p(10., 12.), 3.));
    s.put(&cell_layer, text("ByBlock, insert 80%", o.p(75., 12.), 3.));

    s.add("TEXT unicode", |_, o| {
        ["Ελληνικά αβγ", "Русский текст", "中文 汉字", "日本語 かな", "√ ∑ ° ± µ Ø"]
            .into_iter()
            .enumerate()
            .map(|(i, t)| text(t, o.p(10., 75. - i as f64 * 14.), 5.))
            .collect()
    });
    s.add("REGION", |_, o| {
        let star: Vec<Vector3> = (0..10)
            .map(|i| {
                let (r, a) = (if i % 2 == 0 { 30. } else { 12. }, PI / 2. + i as f64 * PI / 5.);
                o.p(40. + r * a.cos(), 45. + r * a.sin())
            })
            .collect();
        let l_shape = [(80., 15.), (130., 15.), (130., 35.), (100., 35.), (100., 75.), (80., 75.)].map(|(x, y)| o.p(x, y));
        [star, l_shape.to_vec()].iter().map(|pts| EntityType::Region(Region::from_sat(&region_sat(pts)))).collect()
    });
    s.add("MULTILEADER", |doc, o| {
        let style = doc.objects.iter().find_map(|(h, ob)| matches!(ob, ObjectType::MultiLeaderStyle(_)).then_some(*h));
        let mut a = MultiLeader::with_text("Leader note", o.p(60., 70.), vec![o.p(15., 30.), o.p(55., 70.)]);
        let mut b = MultiLeader::with_text("Two lines", o.p(95., 40.), vec![o.p(70., 15.), o.p(90., 40.)]);
        b.context.leader_roots[0].create_line(vec![o.p(130., 15.), o.p(100., 40.)]);
        for m in [&mut a, &mut b] {
            m.style_handle = style;
            m.text_height = 4.;
            m.arrowhead_size = 3.;
        }
        vec![EntityType::MultiLeader(Box::new(a)), EntityType::MultiLeader(Box::new(b))]
    });

    // Layers that are off or frozen, and paper space: none of it may show.
    layer(s, "X_OFF", |l| l.turn_off());
    layer(s, "X_FROZEN", |l| l.freeze());
    let (o, cell_layer) = s.next("HIDDEN off frozen paper");
    s.put("X_OFF", text("LAYER OFF", o.p(10., 60.), 6.));
    s.put("X_FROZEN", text("LAYER FROZEN", o.p(10., 40.), 6.));
    s.put(&cell_layer, text("Only this label should show", o.p(5., 10.), 3.));
    let mut paper = Text::with_value("PAPER SPACE ONLY", v(20., 20.)).with_height(6.);
    paper.common.layer = cell_layer;
    s.doc.add_paper_space_entity(EntityType::Text(paper)).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir] = &args[..] else {
        panic!("usage: make_baseline OUT_DIR");
    };
    let mut s = Sheet { doc: CadDocument::new(), top_left: v(0., 0.), count: 0 };
    setup(&mut s.doc);
    for (name, color) in [("X_LABELS", 7), ("X_FRAMES", 8)] {
        s.layer(name, color);
    }
    s.label("OPEN DRAWING VIEWER TEST DRAWING (make_baseline)", s.top_left + v(0., 15.), 12.);
    base_cells(&mut s);
    build(&mut s);
    println!("{} cells, {} entities", s.count, s.doc.entity_count());
    for ext in ["dxf", "dwg"] {
        save(&mut s.doc, &format!("{dir}/baseline.{ext}"));
    }
}

