//! Dimensions drawn from their definition points, for files whose dimensions have no (or an empty)
//! anonymous block, and TOLERANCE feature control frames. Sizes come from the dimension style;
//! the layout follows AutoCAD's defaults closely enough to read, not to the pixel.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::{arrow, parse, sweep, Affine, Pen, World, P};

/// The dimension style variables used here. Lengths are as stored; `scaled()` applies DIMSCALE.
#[derive(Clone)]
pub(crate) struct DimStyle {
    /// DIMASZ, DIMTXT, DIMEXO, DIMEXE, DIMGAP.
    pub arrow: f64,
    pub text: f64,
    pub ext_offset: f64,
    pub ext_beyond: f64,
    pub gap: f64,
    /// DIMTAD ≠ 0: text above the dimension line instead of on it.
    pub above: bool,
    /// DIMTIH: text inside the extension lines is horizontal instead of along the dimension line.
    pub horizontal: bool,
    /// DIMLFAC, DIMDEC, DIMADEC, DIMZIN, DIMDSEP and DIMPOST.
    pub lfac: f64,
    pub dec: i64,
    pub adec: i64,
    pub zin: i64,
    pub sep: char,
    pub post: String,
    /// DIMSCALE.
    pub scale: f64,
}

impl Default for DimStyle {
    /// AutoCAD's imperial defaults.
    fn default() -> Self {
        DimStyle { arrow: 0.18, text: 0.18, ext_offset: 0.0625, ext_beyond: 0.18, gap: 0.09, above: false, horizontal: true, lfac: 1.0, dec: 4, adec: 0, zin: 0, sep: '.', post: String::new(), scale: 1.0 }
    }
}

impl DimStyle {
    /// The style with its lengths multiplied by DIMSCALE (0 meaning 1, as in model space).
    pub(crate) fn scaled(&self) -> Self {
        let k = if self.scale > 0.0 { self.scale } else { 1.0 };
        let mut s = DimStyle { scale: 1.0, ..self.clone() };
        for v in [&mut s.arrow, &mut s.text, &mut s.ext_offset, &mut s.ext_beyond, &mut s.gap] {
            *v *= k;
        }
        s
    }

    /// A style from DIMSTYLE group codes (or the header variables they stand for), falling back to `base`.
    pub(crate) fn from_codes(var: impl Fn(i32) -> Option<f64>, post: Option<&str>, base: &DimStyle) -> Self {
        let f = |code, default: f64| var(code).filter(|v| v.is_finite()).unwrap_or(default);
        let flag = |code, default: bool| f(code, default as i64 as f64) != 0.0;
        DimStyle {
            arrow: f(41, base.arrow),
            text: f(140, base.text),
            ext_offset: f(42, base.ext_offset),
            ext_beyond: f(44, base.ext_beyond),
            gap: f(147, base.gap),
            above: flag(77, base.above),
            horizontal: flag(73, base.horizontal),
            lfac: f(144, base.lfac),
            dec: f(271, base.dec as f64) as i64,
            adec: f(179, base.adec as f64) as i64,
            zin: f(78, base.zin as f64) as i64,
            sep: var(278).and_then(|c| char::from_u32(c as u32)).filter(|&c| c > ' ').unwrap_or(base.sep),
            post: post.map_or_else(|| base.post.clone(), str::to_string),
            scale: f(40, base.scale),
        }
    }
}

/// A dimension's geometry, as its DXF definition points give it.
pub(crate) enum Dim {
    /// Linear (rotated by `angle`) or aligned (`angle` None): extension line origins and a point on the dimension line.
    Linear { ext1: P, ext2: P, line: P, angle: Option<f64> },
    Radius { centre: P, on_curve: P },
    /// The two ends of the diameter.
    Diameter { a: P, b: P },
    /// Two lines, each by two points, and a point on the dimension arc.
    Angular2 { l1: [P; 2], l2: [P; 2], arc: P },
    /// The angle at `vertex` between `a` and `b`, measured on the side of the arc through `arc`.
    Angular3 { vertex: P, a: P, b: P, arc: P },
    /// X (`x`) or Y ordinate of `feature` from `origin`, with a leader to `leader`.
    Ordinate { origin: P, feature: P, leader: P, x: bool },
    /// Length of the arc of `centre` from `a` to `b`, dimensioned on the arc through `arc`.
    ArcLength { centre: P, a: P, b: P, arc: P },
    /// Jogged radius: the arc point, the jog, and the shown (override) centre.
    Jogged { centre: P, chord: P, jog: P, shown_centre: P },
}

/// A dimension's text: user text (`<>` is the measurement, "" the measurement alone, " " none),
/// its middle point and rotation if the file gives them.
pub(crate) struct DimText<'a> {
    pub user: &'a str,
    pub at: Option<P>,
    pub rotation: Option<f64>,
}

fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
fn mul(a: P, k: f64) -> P {
    [a[0] * k, a[1] * k]
}
fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn len(a: P) -> f64 {
    a[0].hypot(a[1])
}
fn unit(a: P) -> P {
    let l = len(a);
    if l > 0.0 { mul(a, 1.0 / l) } else { [1.0, 0.0] }
}
fn polar(angle: f64) -> P {
    let (s, c) = angle.sin_cos();
    [c, s]
}
fn angle(a: P) -> f64 {
    a[1].atan2(a[0])
}

/// `angle` turned by half a turn if needed so text along it reads left to right or bottom to top.
fn readable(angle: f64) -> f64 {
    let a = angle.rem_euclid(TAU);
    if a > FRAC_PI_2 + 1e-9 && a <= 3.0 * FRAC_PI_2 + 1e-9 { a - PI } else { a }
}

/// `value` with `dec` decimals, the decimal separator and DIMZIN's trailing (8) and leading (4) zero suppression.
fn number(value: f64, dec: i64, zin: i64, sep: char) -> String {
    let mut s = format!("{:.*}", dec.clamp(0, 8) as usize, value);
    if zin & 8 != 0 && s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if zin & 4 != 0 {
        s = s.replacen("0.", ".", 1);
    }
    if s == "-0" { "0".into() } else { s.replace('.', &sep.to_string()) }
}

impl DimStyle {
    /// The text shown for a measurement already formatted as `value` with `prefix`.
    fn label(&self, user: &str, prefix: &str, value: String) -> Option<String> {
        let value = if self.post.is_empty() {
            value
        } else if self.post.contains("<>") {
            self.post.replace("<>", &value)
        } else {
            value + &self.post
        };
        let measured = format!("{prefix}{value}");
        let text = match user {
            "" => measured,
            " " => return None,
            u if u.contains("<>") => u.replace("<>", &measured),
            u => u.to_string(),
        };
        Some(parse::mtext_plain(&text))
    }

    fn length(&self, user: &str, prefix: &str, value: f64) -> Option<String> {
        self.label(user, prefix, number(value * self.lfac, self.dec, self.zin, self.sep))
    }

    fn degrees(&self, user: &str, radians: f64) -> Option<String> {
        self.label(user, "", number(radians.to_degrees(), self.adec, self.zin, self.sep) + "°")
    }
}

/// Draws dimension `d` with style `s`.
pub(crate) fn draw(w: &mut World, d: &Dim, t: &DimText, s: &DimStyle, m: &Affine, pen: Pen) {
    let line = |w: &mut World, points: Vec<P>| w.path(points, m, pen);
    // An arrowhead at `tip` pointing along `out`.
    let head = |w: &mut World, tip: P, out: P| w.arrowhead(&m.then(&arrow(tip, sub(tip, out), s.arrow)), pen);
    // Text centred on `at` (or `default`), along `along` unless horizontal.
    let label = |w: &mut World, text: Option<String>, default: P, along: f64| {
        let Some(text) = text else { return };
        let rotation = t.rotation.unwrap_or(if s.horizontal { 0.0 } else { readable(along) });
        w.push_text(t.at.unwrap_or(default), rotation, s.text, 1.0, (1, 2), text, m, pen);
    };
    // Where text goes by default: on the line at `mid`, or above it.
    let beside = |mid: P, along: f64| if s.above { add(mid, mul(polar(readable(along) + FRAC_PI_2), s.gap + s.text / 2.0)) } else { mid };
    match *d {
        Dim::Linear { ext1, ext2, line: on, angle: rotation } => {
            let dir = rotation.map_or_else(|| unit(sub(ext2, ext1)), polar);
            let foot = |p: P| add(on, mul(dir, dot(sub(p, on), dir)));
            let (f1, f2) = (foot(ext1), foot(ext2));
            for (o, f) in [(ext1, f1), (ext2, f2)] {
                let u = sub(f, o);
                if len(u) > 1e-9 {
                    let u = unit(u);
                    line(w, vec![add(o, mul(u, s.ext_offset)), add(f, mul(u, s.ext_beyond))]);
                }
            }
            dimension_line(w, f1, f2, s, m, pen);
            let along = angle(dir);
            label(w, s.length(t.user, "", dot(sub(ext2, ext1), dir).abs()), beside(mul(add(f1, f2), 0.5), along), along);
        }
        Dim::Radius { centre, on_curve } => {
            let u = unit(sub(on_curve, centre));
            let at = t.at.unwrap_or(add(on_curve, mul(u, 2.0 * s.arrow + s.text)));
            line(w, vec![at, on_curve]);
            head(w, on_curve, unit(sub(on_curve, at)));
            label(w, s.length(t.user, "R", len(sub(on_curve, centre))), at, angle(u));
        }
        Dim::Diameter { a, b } => {
            let centre = mul(add(a, b), 0.5);
            dimension_line(w, a, b, s, m, pen);
            if let Some(at) = t.at.filter(|&at| len(sub(at, centre)) > len(sub(a, centre))) {
                let end = if len(sub(at, a)) < len(sub(at, b)) { a } else { b };
                line(w, vec![end, at]);
            }
            let along = angle(sub(b, a));
            label(w, s.length(t.user, "Ø", len(sub(b, a))), beside(centre, along), along);
        }
        Dim::Angular2 { l1, l2, arc } => {
            let (d1, d2) = (unit(sub(l1[1], l1[0])), unit(sub(l2[1], l2[0])));
            let det = d1[0] * d2[1] - d1[1] * d2[0];
            if det.abs() < 1e-12 {
                return;
            }
            let k = ((l2[0][0] - l1[0][0]) * d2[1] - (l2[0][1] - l1[0][1]) * d2[0]) / det;
            let vertex = add(l1[0], mul(d1, k));
            // The quadrant between the lines that holds the arc point.
            let phi = angle(sub(arc, vertex));
            let rays = [d1, mul(d1, -1.0)].into_iter().flat_map(|a| [d2, mul(d2, -1.0)].map(move |b| (a, b)));
            let Some((a, b)) = rays.flat_map(|(a, b)| [(a, b), (b, a)]).find(|&(a, b)| {
                let sw = sweep(angle(a), angle(b));
                sw < PI && (phi - angle(a)).rem_euclid(TAU) <= sw
            }) else {
                return;
            };
            angular(w, vertex, a, b, len(sub(arc, vertex)), [l1, l2], t, s, m, pen);
        }
        Dim::Angular3 { vertex, a, b, arc } => {
            let (ua, ub) = (unit(sub(a, vertex)), unit(sub(b, vertex)));
            let phi = angle(sub(arc, vertex));
            let (from, to) = if (phi - angle(ua)).rem_euclid(TAU) <= sweep(angle(ua), angle(ub)) { (ua, ub) } else { (ub, ua) };
            angular(w, vertex, from, to, len(sub(arc, vertex)), [[vertex, a], [vertex, b]], t, s, m, pen);
        }
        Dim::ArcLength { centre, a, b, arc } => {
            let (ua, ub) = (unit(sub(a, centre)), unit(sub(b, centre)));
            let phi = angle(sub(arc, centre));
            let sw_ab = sweep(angle(ua), angle(ub));
            let ((from, to), sw) = if (phi - angle(ua)).rem_euclid(TAU) <= sw_ab { ((ua, ub), sw_ab) } else { ((ub, ua), TAU - sw_ab) };
            let r = len(sub(arc, centre));
            arc_line(w, centre, from, to, r, s, m, pen);
            for (p, u) in [(a, ua), (b, ub)] {
                let e = add(centre, mul(u, r));
                let dir = unit(sub(e, p));
                if len(sub(e, p)) > 1e-9 {
                    line(w, vec![add(p, mul(dir, s.ext_offset)), add(e, mul(dir, s.ext_beyond))]);
                }
            }
            let mid = angle(from) + sw / 2.0;
            let default = add(centre, mul(polar(mid), r + s.gap + s.text / 2.0));
            label(w, s.length(t.user, "⌒", len(sub(a, centre)) * sw), default, mid - FRAC_PI_2);
        }
        Dim::Ordinate { origin, feature, leader, x } => {
            // Work in (along, across) the leader's axis: Y for an X ordinate, X for a Y ordinate.
            let (k, j) = if x { (1, 0) } else { (0, 1) };
            let pt = |along: f64, across: f64| {
                let mut p = [0.0; 2];
                p[k] = along;
                p[j] = across;
                p
            };
            let sign = if leader[k] >= feature[k] { 1.0 } else { -1.0 };
            let Some(text) = s.length(t.user, "", (feature[j] - origin[j]).abs()) else { return };
            // The text sits past the leader end (or where the file puts it); AutoCAD then runs the
            // leader under the whole text, above it with DIMTAD, doglegging over 2 arrow sizes that
            // end 2 arrow sizes before the text. Digits are taken as 0.78 of the height wide (Arial).
            let width = 0.78 * s.text * text.chars().count() as f64;
            let at = t.at.unwrap_or(pt(leader[k] + sign * width / 2.0, leader[j]));
            // Text reads along the leader, so its up is −X on an X ordinate and +Y on a Y ordinate.
            let below = if s.above { s.gap + s.text / 2.0 } else { 0.0 };
            let across = at[j] + if x { below } else { -below };
            let first = at[k] - sign * width / 2.0;
            let mut path = vec![pt(feature[k] + sign * s.ext_offset, feature[j])];
            if (across - feature[j]).abs() > 1e-9 {
                path.extend([pt(first - sign * 4.0 * s.arrow, feature[j]), pt(first - sign * 2.0 * s.arrow, across)]);
            }
            path.push(pt(first + sign * width, across));
            line(w, path);
            let along = if x { FRAC_PI_2 } else { 0.0 };
            w.push_text(at, t.rotation.unwrap_or(along), s.text, 1.0, (1, 2), text, m, pen);
        }
        Dim::Jogged { centre, chord, jog, shown_centre } => {
            let u = unit(sub(chord, centre));
            let foot = add(centre, mul(u, dot(sub(jog, centre), u)));
            line(w, vec![shown_centre, jog, foot, chord]);
            head(w, chord, u);
            label(w, s.length(t.user, "R", len(sub(chord, centre))), add(chord, mul(u, 2.0 * s.arrow + s.text)), angle(u));
        }
    }
}

/// A dimension line from `a` to `b` with arrowheads at both ends, outside if they do not fit between.
fn dimension_line(w: &mut World, a: P, b: P, s: &DimStyle, m: &Affine, pen: Pen) {
    let u = unit(sub(b, a));
    if len(sub(b, a)) >= 2.5 * s.arrow {
        w.path(vec![a, b], m, pen);
        for (tip, out) in [(a, mul(u, -1.0)), (b, u)] {
            w.arrowhead(&m.then(&arrow(tip, sub(tip, out), s.arrow)), pen);
        }
    } else {
        w.path(vec![sub(a, mul(u, 2.0 * s.arrow)), add(b, mul(u, 2.0 * s.arrow))], m, pen);
        for (tip, out) in [(a, u), (b, mul(u, -1.0))] {
            w.arrowhead(&m.then(&arrow(tip, sub(tip, out), s.arrow)), pen);
        }
    }
}

/// The arc of radius `r` about `centre` from direction `from` anticlockwise to `to`, with arrowheads.
#[allow(clippy::too_many_arguments)]
fn arc_line(w: &mut World, centre: P, from: P, to: P, r: f64, s: &DimStyle, m: &Affine, pen: Pen) {
    let (t0, sw) = (angle(from), sweep(angle(from), angle(to)));
    w.circle_arc(centre, r, t0, sw, m, pen);
    let t1 = t0 + sw;
    for (tip, out) in [(add(centre, mul(polar(t0), r)), polar(t0 - FRAC_PI_2)), (add(centre, mul(polar(t1), r)), polar(t1 + FRAC_PI_2))] {
        w.arrowhead(&m.then(&arrow(tip, sub(tip, out), s.arrow)), pen);
    }
}

/// An angular dimension at `vertex` from ray `from` anticlockwise to `to`, on radius `r`, with
/// extension lines from the nearer end of each measured line (`lines`, in ray order) where the arc misses it.
#[allow(clippy::too_many_arguments)]
fn angular(w: &mut World, vertex: P, from: P, to: P, r: f64, lines: [[P; 2]; 2], t: &DimText, s: &DimStyle, m: &Affine, pen: Pen) {
    arc_line(w, vertex, from, to, r, s, m, pen);
    for dir in [from, to] {
        // The measured line along this ray, by how well it lines up.
        let l = lines.iter().max_by(|a, b| dot(unit(sub(a[1], a[0])), dir).abs().total_cmp(&dot(unit(sub(b[1], b[0])), dir).abs())).unwrap();
        let (ta, tb) = (dot(sub(l[0], vertex), dir), dot(sub(l[1], vertex), dir));
        let near = if (ta - r).abs() < (tb - r).abs() { ta } else { tb };
        if r > ta.max(tb) + 1e-9 || r < ta.min(tb) - 1e-9 {
            let sign = if r > near { 1.0 } else { -1.0 };
            w.path(vec![add(vertex, mul(dir, near + sign * s.ext_offset)), add(vertex, mul(dir, r + sign * s.ext_beyond))], m, pen);
        }
    }
    let sw = sweep(angle(from), angle(to));
    let mid = angle(from) + sw / 2.0;
    let Some(text) = s.degrees(t.user, sw) else { return };
    let default = add(vertex, mul(polar(mid), r + s.gap + s.text / 2.0));
    let rotation = t.rotation.unwrap_or(if s.horizontal { 0.0 } else { readable(mid - FRAC_PI_2) });
    w.push_text(t.at.unwrap_or(default), rotation, s.text, 1.0, (1, 2), text, m, pen);
}

/// GD&T symbols by their letter in AutoCAD's GDT font.
fn gdt(c: char) -> char {
    match c.to_ascii_lowercase() {
        'a' => '∠',
        'b' => '⊥',
        'c' => '⏥',
        'd' => '⌓',
        'e' => '○',
        'f' => '∥',
        'g' => '⌭',
        'h' => '↗',
        'i' => '⌯',
        'j' => '⌖',
        'k' => '⌒',
        'l' => 'Ⓛ',
        'm' => 'Ⓜ',
        'n' => '⌀',
        'p' => 'Ⓟ',
        'r' => '◎',
        's' => 'Ⓢ',
        't' => '⌰',
        'u' => '⏤',
        c => c,
    }
}

/// The cells of a feature control frame, row by row: `^J` separates rows and `%%v` cells;
/// `{\Fgdt;x}` is GD&T symbol x.
fn frame_cells(text: &str) -> Vec<Vec<String>> {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("{\\Fgdt;") {
        plain.push_str(&rest[..i]);
        let tail = &rest[i + 7..];
        let end = tail.find('}').unwrap_or(tail.len());
        plain.extend(tail[..end].chars().map(gdt));
        rest = tail.get(end + 1..).unwrap_or("");
    }
    plain.push_str(rest);
    plain.split("^J").map(|row| row.split("%%v").map(|cell| parse::mtext_plain(cell).trim().to_string()).collect()).collect()
}

/// A TOLERANCE: boxes of height twice the text, the first row's left middle at `at`, along `dir`.
pub(crate) fn tolerance(w: &mut World, at: P, dir: P, text: &str, s: &DimStyle, m: &Affine, pen: Pen) {
    let h = s.text;
    let tall = 2.0 * h;
    let m = m.then(&Affine::translate(at)).then(&Affine::rotate(angle(dir)));
    for (row, cells) in frame_cells(text).iter().enumerate() {
        let (top, bottom) = (tall / 2.0 - row as f64 * tall, -tall / 2.0 - row as f64 * tall);
        let mut x = 0.0;
        for cell in cells.iter().filter(|c| !c.is_empty()) {
            // A lone symbol gets a square box; text about 0.9 of its height per character.
            let n = cell.chars().count() as f64;
            let width = if n <= 1.0 { tall } else { n * 0.9 * h + h };
            w.path(vec![[x, bottom], [x + width, bottom], [x + width, top], [x, top], [x, bottom]], &m, pen);
            w.push_text([x + width / 2.0, (top + bottom) / 2.0], 0.0, h, 1.0, (1, 2), cell.clone(), &m, pen);
            x += width;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_dimzin_and_separator() {
        assert_eq!(number(12.5, 2, 0, '.'), "12.50");
        assert_eq!(number(12.5, 2, 8, ','), "12,5");
        assert_eq!(number(12.0, 2, 8, '.'), "12");
        assert_eq!(number(0.5, 2, 12, '.'), ".5");
    }

    #[test]
    fn user_text_replaces_or_wraps_measurement() {
        let s = DimStyle { dec: 1, ..DimStyle::default() };
        assert_eq!(s.length("", "", 10.0).as_deref(), Some("10.0"));
        assert_eq!(s.length("<> TYP", "R", 10.0).as_deref(), Some("R10.0 TYP"));
        assert_eq!(s.length("CUSTOM", "", 10.0).as_deref(), Some("CUSTOM"));
        assert_eq!(s.length(" ", "", 10.0), None);
    }

    /// The Y ordinate of the extended baseline as AutoCAD drew it with ISO-25: text "45" centred at
    /// (128, 60) above the line, so the leader doglegs down to y = 58.125 and runs under the text.
    #[test]
    fn ordinate_leader_doglegs_under_text_above() {
        let s = DimStyle { arrow: 2.5, text: 2.5, ext_offset: 0.625, gap: 0.625, above: true, dec: 0, ..DimStyle::default() };
        let mut w = World::new(Vec::new());
        let d = Dim::Ordinate { origin: [20.0, 15.0], feature: [90.0, 60.0], leader: [120.0, 60.0], x: false };
        let t = DimText { user: "", at: Some([128.0, 60.0]), rotation: None };
        draw(&mut w, &d, &t, &s, &Affine::IDENTITY, Pen { color: 0, layer: 0, width: 0.0, dash: 0 });
        let round = |p: &P| p.map(|v| (v * 100.0).round() / 100.0);
        assert_eq!(w.paths[0].points.iter().map(round).collect::<Vec<_>>(), [[90.63, 60.0], [116.05, 60.0], [121.05, 58.13], [129.95, 58.13]]);
        assert_eq!(w.texts[0].text, "45");
    }

    #[test]
    fn frame_cells_from_gdt_codes() {
        let cells = frame_cells("{\\Fgdt;j}%%v{\\Fgdt;n}0.05{\\Fgdt;m}%%vA^J{\\Fgdt;u}%%v0.1");
        assert_eq!(cells, vec![vec!["⌖", "⌀0.05Ⓜ", "A"], vec!["⏤", "0.1"]]);
    }
}
