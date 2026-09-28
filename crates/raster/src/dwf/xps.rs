//! DWFx: an XPS (OPC) package whose sheets are FixedPage XAML.
//!
//! Pages are found through the package: `_rels/.rels` → FixedDocumentSequence → FixedDocuments →
//! PageContent. Paths (solid colour fill and stroke), glyph runs and image-filled paths are drawn;
//! XPS units (1/96 inch, Y down) become mm. Text is drawn in the viewer's font, not the embedded one.

use std::collections::HashMap;
use std::f64::consts::TAU;

use super::image::{self, Kind};
use super::xml::{self, Node};
use super::zip::Zip;
use crate::dxf::{self, Affine, Pen, World, P, FOREGROUND};
use crate::hpgl::{pen_color, CAP_PER_EM};

const MM_PER_UNIT: f64 = 25.4 / 96.0;
const CURVE_STEPS: usize = 16;
/// Largest turn per segment when flattening arcs.
const ARC_STEP: f64 = TAU / 72.0;

/// Decodes page `page` (0-based) of a DWFx package and returns it with the page count.
pub(super) fn decode(data: &[u8], page: u32) -> Result<(dxf::Drawing, u32), String> {
    let zip = Zip::open(data)?;
    let pages = pages(&zip)?;
    let part = pages.get(page as usize).ok_or_else(|| if pages.is_empty() { "The DWFx package has no pages".into() } else { super::no_page(page) })?;
    let doc = read_xml(&zip, part)?;
    let mut world = World::new(Vec::new());
    let layer = world.layer("Drawing", FOREGROUND);
    let mut d = Drawer { zip: &zip, part, world, resources: HashMap::new(), layer };
    d.collect_resources(&doc);
    d.element(&doc, &Affine::scale(MM_PER_UNIT, -MM_PER_UNIT));
    let mut info = vec![("DWF version".to_string(), "DWFx (XPS)".to_string())];
    if let (Some(w), Some(h)) = (num(doc.attr("Width")), num(doc.attr("Height"))) {
        let mm = |v: f64| super::w2d::round((v * MM_PER_UNIT * 10.0).round() / 10.0);
        info.push(("Paper".into(), format!("{} × {} mm", mm(w), mm(h))));
    }
    d.world.finish("mm", info).map(|drawing| (drawing, pages.len() as u32))
}

fn read_xml(zip: &Zip, part: &str) -> Result<Node, String> {
    xml::parse(&zip.read(part).ok_or_else(|| format!("{part} is missing from the package"))??)
}

/// Page parts in order.
fn pages(zip: &Zip) -> Result<Vec<String>, String> {
    let rels = read_xml(zip, "_rels/.rels").ok();
    let from_rels = rels.iter().flat_map(|r| &r.children).find(|r| r.attr("Type").is_some_and(|t| t.ends_with("/fixedrepresentation"))).and_then(|r| r.attr("Target"));
    let seq = match from_rels {
        Some(target) => resolve("", target),
        None => zip.names().find(|n| n.to_lowercase().ends_with(".fdseq")).ok_or("Not a DWFx (XPS) package")?.to_string(),
    };
    let mut out = Vec::new();
    for docref in read_xml(zip, &seq)?.children.iter().filter(|n| n.name == "DocumentReference") {
        let Some(fdoc) = docref.attr("Source").map(|s| resolve(&seq, s)) else { continue };
        for content in read_xml(zip, &fdoc)?.children.iter().filter(|n| n.name == "PageContent") {
            out.extend(content.attr("Source").map(|s| resolve(&fdoc, s)));
        }
    }
    Ok(out)
}

/// `target` relative to the folder of the part `base`, or from the package root if it starts with `/`.
fn resolve(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { base.split('/').filter(|s| !s.is_empty()).collect() };
    if !target.starts_with('/') {
        parts.pop();
    }
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn num(v: Option<&str>) -> Option<f64> {
    v?.trim().parse().ok()
}

fn numbers(v: &str) -> Vec<f64> {
    v.split([',', ' ']).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

/// `m11,m12,m21,m22,dx,dy`.
fn matrix(v: &str) -> Option<Affine> {
    match numbers(v)[..] {
        [a, b, c, d, e, f] => Some(Affine::new([a, b, c, d, e, f])),
        _ => None,
    }
}

/// `#RRGGBB`, `#AARRGGBB` or `sc#[a,]r,g,b` as 0xRRGGBB; `None` if fully transparent.
fn color(v: &str) -> Option<u32> {
    let v = v.trim();
    if let Some(sc) = v.strip_prefix("sc#") {
        let c = numbers(sc);
        let (a, rgb) = match c[..] {
            [a, r, g, b] => (a, [r, g, b]),
            [r, g, b] => (1.0, [r, g, b]),
            _ => return None,
        };
        let byte = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
        return (a > 0.0).then(|| byte(rgb[0]) << 16 | byte(rgb[1]) << 8 | byte(rgb[2]));
    }
    let hex = u32::from_str_radix(v.strip_prefix('#')?, 16).ok()?;
    match v.len() {
        7 => Some(hex),
        9 => (hex >> 24 != 0).then_some(hex & 0xFF_FFFF),
        _ => None,
    }
}

/// The value of property element `Owner.name`, e.g. the brush inside `<Path.Fill>`.
fn prop<'n>(n: &'n Node, name: &str) -> Option<&'n Node> {
    let full = format!("{}.{name}", n.name);
    n.children.iter().find(|c| c.name == full)?.children.first()
}

#[derive(Clone, Copy)]
struct Figure {
    start: usize,
    len: usize,
    closed: bool,
}

/// Flattened path geometry.
#[derive(Default)]
struct Geometry {
    points: Vec<P>,
    figures: Vec<Figure>,
    even_odd: bool,
}

impl Geometry {
    fn figure(&self, f: &Figure) -> Vec<P> {
        let mut pts = self.points[f.start..f.start + f.len].to_vec();
        if f.closed && pts.len() > 1 {
            pts.push(pts[0]);
        }
        pts
    }

    fn move_to(&mut self, p: P) {
        self.figures.push(Figure { start: self.points.len(), len: 1, closed: false });
        self.points.push(p);
    }

    /// Continues the open figure, or starts one at `from` after a close.
    fn line_to(&mut self, from: P, p: P) {
        if self.figures.last().is_none_or(|f| f.closed) {
            self.move_to(from);
        }
        self.points.push(p);
        self.figures.last_mut().unwrap().len += 1;
    }
}

/// Numbers and commands of the abbreviated path syntax.
struct Scanner<'s> {
    b: &'s [u8],
    i: usize,
}

impl Scanner<'_> {
    fn skip(&mut self) {
        while self.b.get(self.i).is_some_and(|&c| c == b',' || c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.b.get(self.i).copied()
    }

    fn num(&mut self) -> Option<f64> {
        self.skip();
        let start = self.i;
        let mut prev = 0;
        while let Some(&c) = self.b.get(self.i) {
            let ok = c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || ((c == b'-' || c == b'+') && (self.i == start || prev == b'e' || prev == b'E'));
            if !ok {
                break;
            }
            prev = c;
            self.i += 1;
        }
        std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok()
    }

    fn point(&mut self) -> Option<P> {
        Some([self.num()?, self.num()?])
    }
}

fn lerp(a: P, b: P, t: f64) -> P {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn cubic(g: &mut Geometry, p0: P, p1: P, p2: P, p3: P) {
    for k in 1..=CURVE_STEPS {
        let t = k as f64 / CURVE_STEPS as f64;
        let (a, b, c) = (lerp(p0, p1, t), lerp(p1, p2, t), lerp(p2, p3, t));
        g.line_to(p0, lerp(lerp(a, b, t), lerp(b, c, t), t));
    }
}

fn quad(g: &mut Geometry, p0: P, p1: P, p2: P) {
    for k in 1..=CURVE_STEPS {
        let t = k as f64 / CURVE_STEPS as f64;
        g.line_to(p0, lerp(lerp(p0, p1, t), lerp(p1, p2, t), t));
    }
}

/// Elliptical arc from `p0` to `p1` (SVG endpoint parameterization; `sweep` = clockwise on the page).
#[allow(clippy::too_many_arguments)]
fn arc(g: &mut Geometry, p0: P, [mut rx, mut ry]: P, rotation: f64, large: bool, sweep: bool, p1: P) {
    (rx, ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 || p0 == p1 {
        g.line_to(p0, p1);
        return;
    }
    let (s, c) = rotation.to_radians().sin_cos();
    let (dx, dy) = ((p0[0] - p1[0]) / 2.0, (p0[1] - p1[1]) / 2.0);
    let (x, y) = (c * dx + s * dy, -s * dx + c * dy);
    let lambda = x * x / (rx * rx) + y * y / (ry * ry);
    if lambda > 1.0 {
        (rx, ry) = (rx * lambda.sqrt(), ry * lambda.sqrt());
    }
    let num = rx * rx * ry * ry - rx * rx * y * y - ry * ry * x * x;
    let den = rx * rx * y * y + ry * ry * x * x;
    let k = (num / den).max(0.0).sqrt() * if large == sweep { -1.0 } else { 1.0 };
    let (cx, cy) = (k * rx * y / ry, -k * ry * x / rx);
    let centre = [c * cx - s * cy + (p0[0] + p1[0]) / 2.0, s * cx + c * cy + (p0[1] + p1[1]) / 2.0];
    let angle = |ux: f64, uy: f64| uy.atan2(ux);
    let t0 = angle((x - cx) / rx, (y - cy) / ry);
    let mut dt = angle((-x - cx) / rx, (-y - cy) / ry) - t0;
    if sweep && dt < 0.0 {
        dt += TAU;
    } else if !sweep && dt > 0.0 {
        dt -= TAU;
    }
    let steps = (dt.abs() / ARC_STEP).ceil().max(1.0) as usize;
    for i in 1..=steps {
        let t = t0 + dt * i as f64 / steps as f64;
        let (ex, ey) = (rx * t.cos(), ry * t.sin());
        let p = if i == steps { p1 } else { [centre[0] + c * ex - s * ey, centre[1] + s * ex + c * ey] };
        g.line_to(p0, p);
    }
}

/// Parses the abbreviated path syntax (`F1 M 0,0 L 10,0 …`). Stops quietly at anything malformed.
fn path_data(s: &str) -> Geometry {
    let mut g = Geometry { even_odd: true, ..Default::default() };
    let mut t = Scanner { b: s.as_bytes(), i: 0 };
    let (mut cur, mut start, mut ctrl): (P, P, Option<P>) = ([0.0; 2], [0.0; 2], None);
    let mut cmd = b'M';
    while let Some(c) = t.peek() {
        if c.is_ascii_alphabetic() {
            t.i += 1;
            cmd = c;
            match c {
                b'F' => {
                    g.even_odd = t.num() != Some(1.0);
                    continue;
                }
                b'Z' | b'z' => {
                    if let Some(f) = g.figures.last_mut() {
                        f.closed = true;
                    }
                    cur = start;
                    ctrl = None;
                    continue;
                }
                _ => {}
            }
        } else if matches!(cmd, b'Z' | b'z' | b'F') {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let at = |p: P| if rel { [cur[0] + p[0], cur[1] + p[1]] } else { p };
        let reflect = ctrl.map_or(cur, |c| [2.0 * cur[0] - c[0], 2.0 * cur[1] - c[1]]);
        let mut next_ctrl = None;
        let step = (|| -> Option<P> {
            Some(match cmd.to_ascii_uppercase() {
                b'M' => {
                    let p = at(t.point()?);
                    g.move_to(p);
                    start = p;
                    // Further pairs after a move are lines.
                    cmd = if rel { b'l' } else { b'L' };
                    p
                }
                b'L' => {
                    let p = at(t.point()?);
                    g.line_to(cur, p);
                    p
                }
                b'H' => {
                    let x = t.num()?;
                    let p = [if rel { cur[0] + x } else { x }, cur[1]];
                    g.line_to(cur, p);
                    p
                }
                b'V' => {
                    let y = t.num()?;
                    let p = [cur[0], if rel { cur[1] + y } else { y }];
                    g.line_to(cur, p);
                    p
                }
                b'C' | b'S' => {
                    let c1 = if cmd.eq_ignore_ascii_case(&b'C') { at(t.point()?) } else { reflect };
                    let (c2, p) = (at(t.point()?), at(t.point()?));
                    cubic(&mut g, cur, c1, c2, p);
                    next_ctrl = Some(c2);
                    p
                }
                b'Q' | b'T' => {
                    let c1 = if cmd.eq_ignore_ascii_case(&b'Q') { at(t.point()?) } else { reflect };
                    let p = at(t.point()?);
                    quad(&mut g, cur, c1, p);
                    next_ctrl = Some(c1);
                    p
                }
                b'A' => {
                    let size = t.point()?;
                    let (rotation, large, sweep) = (t.num()?, t.num()? != 0.0, t.num()? != 0.0);
                    let p = at(t.point()?);
                    arc(&mut g, cur, size, rotation, large, sweep, p);
                    p
                }
                _ => return None,
            })
        })();
        let Some(p) = step else { break };
        cur = p;
        ctrl = next_ctrl;
    }
    g
}

/// A `PathGeometry` element: its `Figures` attribute and/or `PathFigure` children, as path data.
fn path_geometry(n: &Node) -> Geometry {
    let mut data = n.attr("Figures").unwrap_or_default().to_string();
    let figures = n.children.iter().flat_map(|c| if c.name == "PathGeometry.Figures" { c.children.iter().collect() } else { vec![c] });
    for f in figures.filter(|f| f.name == "PathFigure") {
        data += &format!(" M {}", f.attr("StartPoint").unwrap_or("0,0"));
        for s in &f.children {
            let a = |k| s.attr(k).unwrap_or_default();
            data += &match s.name.as_str() {
                "PolyLineSegment" => format!(" L {}", a("Points")),
                "LineSegment" => format!(" L {}", a("Point")),
                "PolyBezierSegment" => format!(" C {}", a("Points")),
                "BezierSegment" => format!(" C {} {} {}", a("Point1"), a("Point2"), a("Point3")),
                "PolyQuadraticBezierSegment" => format!(" Q {}", a("Points")),
                "QuadraticBezierSegment" => format!(" Q {} {}", a("Point1"), a("Point2")),
                "ArcSegment" => {
                    let large = u8::from(a("IsLargeArc") == "true");
                    let sweep = u8::from(a("SweepDirection") == "Clockwise");
                    format!(" A {} {} {large} {sweep} {}", a("Size"), num(s.attr("RotationAngle")).unwrap_or(0.0), a("Point"))
                }
                _ => String::new(),
            };
        }
        if f.attr("IsClosed") == Some("true") {
            data += " Z";
        }
    }
    let mut g = path_data(&data);
    g.even_odd = n.attr("FillRule") != Some("NonZero");
    if let Some(m) = n.attr("Transform").and_then(matrix) {
        g.points.iter_mut().for_each(|p| *p = m.apply(*p));
    }
    g
}

enum Brush {
    Color(u32),
    Image(Node),
}

struct Drawer<'z> {
    zip: &'z Zip<'z>,
    part: &'z str,
    world: World<'static>,
    /// `x:Key` → resource, from the page's dictionaries (inline and remote).
    resources: HashMap<String, Node>,
    layer: u32,
}

impl Drawer<'_> {
    fn collect_resources(&mut self, page: &Node) {
        for dict in page.descendants().into_iter().filter(|n| n.name == "ResourceDictionary") {
            let remote = dict.attr("Source").and_then(|s| read_xml(self.zip, &resolve(self.part, s)).ok());
            for entry in dict.children.iter().chain(remote.iter().flat_map(|r| &r.children)) {
                if let Some(key) = entry.attr("Key") {
                    self.resources.insert(key.to_string(), entry.clone());
                }
            }
        }
    }

    /// The resource named by `{StaticResource key}`.
    fn lookup(&self, v: &str) -> Option<&Node> {
        let key = v.trim().strip_prefix('{')?.strip_suffix('}')?.trim().strip_prefix("StaticResource")?.trim();
        self.resources.get(key)
    }

    /// An element's own `RenderTransform`, as attribute, resource or property element.
    fn transform(&self, n: &Node) -> Affine {
        let node = match n.attr("RenderTransform") {
            Some(v) if v.trim_start().starts_with('{') => self.lookup(v),
            Some(v) => return matrix(v).unwrap_or(Affine::IDENTITY),
            None => prop(n, "RenderTransform"),
        };
        node.and_then(|t| t.attr("Matrix")).and_then(matrix).unwrap_or(Affine::IDENTITY)
    }

    fn brush(&mut self, n: &Node, name: &str) -> Option<Brush> {
        let node = match n.attr(name) {
            Some(v) if v.trim_start().starts_with('{') => self.lookup(v)?.clone(),
            Some(v) => return color(v).map(Brush::Color),
            None => prop(n, name)?.clone(),
        };
        match node.name.as_str() {
            "SolidColorBrush" => node.attr("Color").and_then(color).map(Brush::Color),
            "ImageBrush" => Some(Brush::Image(node)),
            "LinearGradientBrush" | "RadialGradientBrush" => {
                self.world.skip("Gradient (drawn flat)");
                let stops = node.descendants().into_iter().filter(|s| s.name == "GradientStop").find_map(|s| s.attr("Color").and_then(color));
                stops.map(Brush::Color)
            }
            _ => {
                self.world.skip("Visual brush");
                None
            }
        }
    }

    fn pen(&self, color: u32, width: f64) -> Pen {
        Pen { color: pen_color(color), layer: self.layer, width }
    }

    fn element(&mut self, n: &Node, m: &Affine) {
        match n.name.as_str() {
            "FixedPage" | "Canvas" => {
                let m = m.then(&self.transform(n));
                n.children.iter().for_each(|c| self.element(c, &m));
            }
            "Path" => self.path(n, m),
            "Glyphs" => self.glyphs(n, m),
            _ => {}
        }
    }

    fn path(&mut self, n: &Node, m: &Affine) {
        let m = m.then(&self.transform(n));
        let geometry = match n.attr("Data") {
            Some(v) if v.trim_start().starts_with('{') => self.lookup(v).map(path_geometry),
            Some(v) => Some(path_data(v)),
            None => prop(n, "Data").map(path_geometry),
        };
        let Some(g) = geometry else { return };
        match self.brush(n, "Fill") {
            Some(Brush::Color(c)) => {
                let rings = g.figures.iter().map(|f| g.points[f.start..f.start + f.len].to_vec()).collect();
                let pen = self.pen(c, 0.0);
                self.world.fill(rings, g.even_odd, &m, pen);
            }
            Some(Brush::Image(b)) => self.image(&b, &m),
            None => {}
        }
        if let Some(Brush::Color(c)) = self.brush(n, "Stroke") {
            let [u, v] = [m.linear([1.0, 0.0]), m.linear([0.0, 1.0])];
            let scale = (u[0] * v[1] - u[1] * v[0]).abs().sqrt();
            let pen = self.pen(c, num(n.attr("StrokeThickness")).unwrap_or(1.0) * scale);
            for f in &g.figures {
                self.world.path(g.figure(f), &m, pen);
            }
        }
    }

    fn glyphs(&mut self, n: &Node, m: &Affine) {
        let m = m.then(&self.transform(n));
        let text = n.attr("UnicodeString").map(|s| s.strip_prefix("{}").unwrap_or(s)).unwrap_or_default();
        if text.trim().is_empty() {
            if n.attr("Indices").is_some() {
                self.world.skip("Glyphs without text");
            }
            return;
        }
        let Some(Brush::Color(c)) = self.brush(n, "Fill") else { return };
        let origin = [num(n.attr("OriginX")).unwrap_or(0.0), num(n.attr("OriginY")).unwrap_or(0.0)];
        let em = num(n.attr("FontRenderingEmSize")).unwrap_or(0.0);
        // Baseline and up directions on the page (Y down), in the output's Y-up world.
        let (x, up) = (m.linear([1.0, 0.0]), m.linear([0.0, -1.0]));
        let (width, height) = (x[0].hypot(x[1]), up[0].hypot(up[1]));
        if height == 0.0 {
            return;
        }
        let pen = self.pen(c, 0.0);
        let rotation = x[1].atan2(x[0]);
        self.world.push_text(m.apply(origin), rotation, em * height * CAP_PER_EM, width / height, (0, 0), text.to_string(), &Affine::IDENTITY, pen);
    }

    /// An image brush filling its `Viewport` (absolute units, the whole image, no tiling).
    fn image(&mut self, b: &Node, m: &Affine) {
        let Some(src) = b.attr("ImageSource").filter(|s| !s.starts_with('{')) else {
            self.world.skip("Image (colour-managed)");
            return;
        };
        let decoded = match self.zip.read(&resolve(self.part, src)) {
            Some(Ok(data)) if data.starts_with(b"\x89PNG") => image::decode(Kind::Png, 0, 0, &[], &data),
            Some(Ok(data)) if data.starts_with(&[0xFF, 0xD8]) => image::decode(Kind::Jpeg, 0, 0, &[], &data),
            Some(Ok(data)) if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") => crate::tiff::decode(&data, 0).map(|(i, _)| i),
            _ => Err(String::new()),
        };
        let Ok(img) = decoded else {
            self.world.skip("Image (missing or unsupported)");
            return;
        };
        let [x, y, w, _] = numbers(b.attr("Viewport").unwrap_or_default())[..] else { return };
        let m = m.then(&b.attr("Transform").and_then(matrix).unwrap_or(Affine::IDENTITY));
        let px = m.linear([w / img.size().0 as f64, 0.0]);
        self.world.image(img, m.apply([x, y]), px[0].hypot(px[1]), self.layer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwf::zip::tests::build;

    fn close(a: P, b: P) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn resolves_part_names() {
        assert_eq!(resolve("Documents/1/FixedDoc.fdoc", "Pages/1.fpage"), "Documents/1/Pages/1.fpage");
        assert_eq!(resolve("Documents/1/Pages/1.fpage", "../../Resources/a.png"), "Documents/Resources/a.png");
        assert_eq!(resolve("x/y.fdseq", "/Doc/z.fdoc"), "Doc/z.fdoc");
        assert_eq!(resolve("", "FixedDocSeq.fdseq"), "FixedDocSeq.fdseq");
    }

    #[test]
    fn colors() {
        assert_eq!(color("#FF0000"), Some(0xFF0000));
        assert_eq!(color("#80112233"), Some(0x112233));
        assert_eq!(color("#00112233"), None);
        assert_eq!(color("sc#1,0,1,0"), Some(0x00FF00));
    }

    #[test]
    fn path_syntax() {
        let g = path_data("F1 M 0,0 L 10,0 h 5 v 5 Z m 1,1 l 1,0 1,1");
        assert!(!g.even_odd);
        assert_eq!(g.figures.len(), 2);
        assert!(g.figures[0].closed);
        assert_eq!(g.figure(&g.figures[0]), vec![[0.0, 0.0], [10.0, 0.0], [15.0, 0.0], [15.0, 5.0], [0.0, 0.0]]);
        // After Z the current point is the figure start, so "m 1,1" moves to (1, 1).
        assert_eq!(g.figure(&g.figures[1]), vec![[1.0, 1.0], [2.0, 1.0], [3.0, 2.0]]);
        let g = path_data("M0-5L1e1,2.5");
        assert_eq!(g.points, vec![[0.0, -5.0], [10.0, 2.5]]);
    }

    #[test]
    fn curves_end_on_their_endpoints() {
        let g = path_data("M 0,0 C 0,10 10,10 10,0 Q 15,5 20,0 A 5,5 0 0 1 30,0");
        assert!(close(g.points[CURVE_STEPS], [10.0, 0.0]));
        assert!(close(g.points[2 * CURVE_STEPS], [20.0, 0.0]));
        assert!(close(*g.points.last().unwrap(), [30.0, 0.0]));
        // A clockwise half circle on the page (Y down) bulges towards negative Y.
        assert!(g.points[2 * CURVE_STEPS..].iter().all(|p| p[1] <= 1e-9));
        assert!(g.points.iter().any(|p| (p[1] + 5.0).abs() < 1e-6));
    }

    #[test]
    fn path_geometry_elements() {
        let doc = xml::parse(
            br#"<PathGeometry FillRule="NonZero"><PathFigure StartPoint="0,0" IsClosed="true"><PolyLineSegment Points="10,0 10,10"/></PathFigure></PathGeometry>"#,
        )
        .unwrap();
        let g = path_geometry(&doc);
        assert!(!g.even_odd);
        assert_eq!(g.figure(&g.figures[0]).len(), 4);
    }

    fn dwfx(page: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let rels = r#"<Relationships><Relationship Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation" Target="/FixedDocSeq.fdseq"/></Relationships>"#;
        let seq = r#"<FixedDocumentSequence><DocumentReference Source="Documents/1/FixedDoc.fdoc"/></FixedDocumentSequence>"#;
        let doc = r#"<FixedDocument><PageContent Source="Pages/1.fpage"/><PageContent Source="Pages/2.fpage"/></FixedDocument>"#;
        let blank = r##"<FixedPage Width="96" Height="96"><Path Data="M 0,0 L 1,1" Stroke="#000000"/></FixedPage>"##;
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("_rels/.rels", rels.as_bytes()),
            ("FixedDocSeq.fdseq", seq.as_bytes()),
            ("Documents/1/FixedDoc.fdoc", doc.as_bytes()),
            ("Documents/1/Pages/1.fpage", page.as_bytes()),
            ("Documents/1/Pages/2.fpage", blank.as_bytes()),
        ];
        entries.extend(extra);
        build(&entries, true)
    }

    #[test]
    fn fixed_page() {
        let page = r##"<FixedPage Width="960" Height="480" xmlns="http://schemas.microsoft.com/xps/2005/06">
            <FixedPage.Resources><ResourceDictionary><SolidColorBrush x:Key="red" Color="#FFFF0000"/></ResourceDictionary></FixedPage.Resources>
            <Canvas RenderTransform="1,0,0,1,96,0">
                <Path Data="M 0,0 L 96,0 L 96,96 Z" Fill="{StaticResource red}" Stroke="#FF000000" StrokeThickness="2"/>
                <Glyphs UnicodeString="Hello" OriginX="0" OriginY="96" FontRenderingEmSize="96" Fill="#000000"/>
            </Canvas>
            <Path Data="M 0,0 L 48,0 48,48 0,48 Z"><Path.Fill><ImageBrush ImageSource="../Resources/a.png" Viewport="0,0,48,48" ViewportUnits="Absolute"/></Path.Fill></Path>
        </FixedPage>"##;
        let png = crate::dwf::image::tests::png_file(2, 2, 8, 0, &[], &[0, 0, 255, 0, 255, 0]);
        let data = dwfx(page, &[("Documents/1/Resources/a.png", &png)]);
        let (d, pages) = decode(&data, 0).unwrap();
        assert_eq!(pages, 2);
        assert_eq!(d.units, "mm");
        assert_eq!(d.fills.len(), 1);
        assert_eq!(d.fills[0].color, 0xFF0000);
        assert_eq!(d.paths.len(), 1);
        assert!((d.paths[0].width - 2.0 * MM_PER_UNIT).abs() < 1e-9);
        assert_eq!(d.texts[0].text, "Hello");
        let cap = d.texts[0].up[0].hypot(d.texts[0].up[1]);
        assert!((cap - 25.4 * CAP_PER_EM).abs() < 1e-9);
        assert_eq!(d.images.len(), 1);
        assert!((d.images[0].px - 24.0 * MM_PER_UNIT).abs() < 1e-9);
        assert!(d.info.contains(&("Paper".into(), "254 × 127 mm".into())));
        assert!(decode(&data, 2).is_err());
    }
}
