//! HP-GL, HP-GL/2 and HP RTL plot files, drawn through the DXF output helpers.
//!
//! Plotter units (1/40 mm) are scaled to millimetres. Arcs, circles, wedges and Béziers are
//! flattened; each pen becomes a layer. RTL raster images are placed from the top-left corner of
//! the page, whose height comes from `PS` or else the top of the vector drawing.

mod lex;
mod pcl;

use std::collections::BTreeMap;
use std::f64::consts::TAU;

use crate::dxf::{self, Affine, Pen, World, BACKGROUND, FOREGROUND, P};
use lex::{Command, Lexer, Pe};
use pcl::{Event, RtlImage};

const MM_PER_PLU: f64 = 0.025;
const PLU_PER_CM: f64 = 400.0;
const PLU_PER_INCH: f64 = 1016.0;
const MM_PER_INCH: f64 = 25.4;
/// Largest turn per segment when flattening arcs: finer than a plotter's chords, so curves stay
/// smooth when zoomed in.
const ARC_STEP: f64 = TAU / 360.0;
const MAX_ARC_STEPS: f64 = 36_000.0;
const BEZIER_STEPS: usize = 64;
const PENS: usize = 256;
/// Default pen width in mm (HP-GL/2).
const PEN_WIDTH: f64 = 0.35;
/// Default character size (SI) in cm: width and cap height.
const CHAR_SIZE: P = [0.19, 0.27];
/// Default relative character size (SR) in percent of P2 − P1.
const CHAR_SIZE_RELATIVE: P = [0.75, 1.5];
/// Advance of the canvas' sans-serif per cap height, to turn HP-GL character spacing into a width factor.
const ADVANCE_PER_CAP: f64 = 0.86;
/// Cap height per point size of a font.
pub(crate) const CAP_PER_EM: f64 = 0.7;
/// Scaling points P1, P2 of an A-size plotter, when the file sets no plot size.
const P1: P = [250.0, 596.0];
const P2: P = [10250.0, 7796.0];
/// Pen colours 0–7: white, black, red, green, yellow, blue, magenta, cyan. Higher pens repeat 1–7.
const PEN_COLORS: [u32; 8] = [BACKGROUND, FOREGROUND, 0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF];

/// Decodes page `page` (0-based; pages are separated by `PG`) and returns it with the page count.
pub fn decode(data: &[u8], page: u32) -> Result<(dxf::Drawing, u32), String> {
    let mut d = Decoder::new(page);
    let wrapped = pcl::split(data, |e| d.event(e));
    d.flush();
    let pages = (d.page + d.drawn as u32).max(1);
    if page >= pages {
        return Err(format!("Page {} does not exist", page + 1));
    }
    let info = d.info(wrapped);
    d.place_images();
    d.world.finish("mm", info).map(|drawing| (drawing, pages))
}

fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1]]
}

fn polar(centre: P, r: f64, angle: f64) -> P {
    let (s, c) = angle.sin_cos();
    [centre[0] + r * c, centre[1] + r * s]
}

/// Segments for an arc turning by `sweep` radians.
fn arc_steps(sweep: f64) -> usize {
    (sweep.abs() / ARC_STEP).ceil().clamp(1.0, MAX_ARC_STEPS) as usize
}

fn closed(mut ring: Vec<P>) -> Vec<P> {
    if let Some(&first) = ring.first() {
        ring.push(first);
    }
    ring
}

/// Pen colour for an RGB value; black and white become the foreground and paper colours so they
/// follow the viewer's invert setting.
pub(crate) fn pen_color(rgb: u32) -> u32 {
    match rgb {
        0 => FOREGROUND,
        0xFFFFFF => BACKGROUND,
        rgb => rgb,
    }
}

fn default_color(pen: usize) -> u32 {
    if pen == 0 { PEN_COLORS[0] } else { PEN_COLORS[1 + (pen - 1) % 7] }
}

/// Graphics state, reset by `IN`.
struct Plotter {
    /// Pen position in plotter units.
    pos: P,
    down: bool,
    relative: bool,
    p1: P,
    p2: P,
    /// `SC` parameters, applied through `map`.
    sc: Vec<f64>,
    /// User units to plotter units: `a·u + b` per axis.
    map: (P, P),
    pen: usize,
    colors: Vec<u32>,
    /// In mm.
    widths: Vec<f64>,
    relative_widths: bool,
    /// `CR` colour range per primary.
    range: [P; 3],
    fill_type: u32,
    /// Shading level (FT 10) in percent.
    shade: f64,
    /// Character width and cap height in plotter units.
    char_size: P,
    /// Label direction in radians.
    direction: f64,
    /// Label origin (LO) 1–9.
    origin: u32,
    /// Subpolygons while in polygon mode.
    polygon: Option<Vec<Vec<P>>>,
    /// The polygon buffer for EP and FP.
    buffer: Vec<Vec<P>>,
    /// RO, in radians.
    rotation: f64,
}

impl Plotter {
    fn new(plot_size: Option<P>) -> Self {
        let (p1, p2) = plot_size.map_or((P1, P2), |s| ([0.0; 2], s));
        let mut p = Plotter {
            pos: [0.0; 2],
            down: false,
            relative: false,
            p1,
            p2,
            sc: Vec::new(),
            map: ([1.0; 2], [0.0; 2]),
            pen: 1,
            colors: (0..PENS).map(default_color).collect(),
            widths: vec![PEN_WIDTH; PENS],
            relative_widths: false,
            range: [[0.0, 255.0]; 3],
            fill_type: 1,
            shade: 100.0,
            char_size: [0.0; 2],
            direction: 0.0,
            origin: 1,
            polygon: None,
            buffer: Vec::new(),
            rotation: 0.0,
        };
        p.defaults();
        p
    }

    /// `DF`: scaling, text, fill and polygon state; pens, P1/P2 and rotation are kept.
    fn defaults(&mut self) {
        self.relative = false;
        self.sc.clear();
        self.rescale();
        self.fill_type = 1;
        self.shade = 100.0;
        self.char_size = CHAR_SIZE.map(|v| v * PLU_PER_CM);
        self.direction = 0.0;
        self.origin = 1;
        self.polygon = None;
        self.buffer.clear();
    }

    fn span(&self) -> P {
        [self.p2[0] - self.p1[0], self.p2[1] - self.p1[1]]
    }

    /// Recomputes `map` from `SC` (anisotropic, isotropic or point factor) and P1/P2.
    fn rescale(&mut self) {
        self.map = ([1.0; 2], [0.0; 2]);
        let s = &self.sc;
        if s.len() < 4 {
            return;
        }
        let (p1, d) = (self.p1, self.span());
        let (a, b) = if s.get(4) == Some(&2.0) {
            let a = [s[1], s[3]];
            (a, [p1[0] - s[0] * a[0], p1[1] - s[2] * a[1]])
        } else {
            let (lo, extent) = ([s[0], s[2]], [s[1] - s[0], s[3] - s[2]]);
            let mut a = [d[0] / extent[0], d[1] / extent[1]];
            let mut spare = [0.0; 2];
            if s.get(4) == Some(&1.0) {
                let m = a[0].abs().min(a[1].abs());
                a = [m.copysign(a[0]), m.copysign(a[1])];
                let at = [s.get(5).copied().unwrap_or(50.0) / 100.0, s.get(6).copied().unwrap_or(50.0) / 100.0];
                spare = [0, 1].map(|k| (d[k] - extent[k] * a[k]) * at[k]);
            }
            (a, [0, 1].map(|k| p1[k] + spare[k] - lo[k] * a[k]))
        };
        if a.iter().chain(&b).all(|v| v.is_finite()) && a[0] != 0.0 && a[1] != 0.0 {
            self.map = (a, b);
        }
    }

    /// User units to plotter units.
    fn plu(&self, u: P) -> P {
        let (a, b) = self.map;
        [a[0] * u[0] + b[0], a[1] * u[1] + b[1]]
    }

    fn user(&self, p: P) -> P {
        let (a, b) = self.map;
        [(p[0] - b[0]) / a[0], (p[1] - b[1]) / a[1]]
    }

    /// A relative move in user units, in plotter units.
    fn vec(&self, v: P) -> P {
        let (a, _) = self.map;
        [a[0] * v[0], a[1] * v[1]]
    }

    fn level(&self, k: usize, v: f64) -> u32 {
        let [lo, hi] = self.range[k];
        if hi == lo { 0 } else { ((v - lo) / (hi - lo) * 255.0).round().clamp(0.0, 255.0) as u32 }
    }

    /// Character spacing and line pitch in plotter units.
    fn text_pitch(&self) -> P {
        [1.5 * self.char_size[0], 2.0 * self.char_size[1]]
    }
}

struct Decoder {
    world: World<'static>,
    st: Plotter,
    /// Pen-down polyline being drawn, in plotter units.
    line: Vec<P>,
    lexer: Lexer,
    /// Page to draw.
    want: u32,
    /// Page being read.
    page: u32,
    /// Something was drawn on the page being read.
    drawn: bool,
    /// `PS` length and width in plotter units.
    plot_size: Option<P>,
    hpgl: bool,
    hpgl2: bool,
    /// Pen number → layer, for the pens drawn with.
    pens: BTreeMap<usize, u32>,
    /// Highest Y drawn, in mm.
    top: f64,
    images: Vec<RtlImage>,
}

impl Decoder {
    fn new(want: u32) -> Self {
        Decoder {
            world: World::new(Vec::new()),
            st: Plotter::new(None),
            line: Vec::new(),
            lexer: Lexer::default(),
            want,
            page: 0,
            drawn: false,
            plot_size: None,
            hpgl: false,
            hpgl2: false,
            pens: BTreeMap::new(),
            top: f64::NEG_INFINITY,
            images: Vec::new(),
        }
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Hpgl(data) => {
                // The lexer calls back into `self`, so it is moved out for the call.
                let mut lexer = std::mem::take(&mut self.lexer);
                lexer.run(data, |c| self.command(c));
                self.lexer = lexer;
            }
            Event::Image(image) => {
                if self.visible() {
                    self.images.push(image);
                }
            }
            Event::Skip(why) => {
                if self.page == self.want {
                    self.world.skip(why);
                }
            }
        }
    }

    /// Marks the page as drawn on; true if it is the page wanted.
    fn visible(&mut self) -> bool {
        self.drawn = true;
        self.page == self.want
    }

    fn out(&self) -> Affine {
        Affine::scale(MM_PER_PLU, MM_PER_PLU).then(&Affine::rotate(self.st.rotation))
    }

    fn grow(&mut self, points: impl IntoIterator<Item = P>) {
        let out = self.out();
        for p in points {
            self.top = self.top.max(out.apply(p)[1]);
        }
    }

    fn pen(&mut self) -> Pen {
        let n = self.st.pen;
        let color = self.st.colors[n];
        let layer = *self.pens.entry(n).or_insert_with(|| self.world.layer(&format!("Pen {n}"), color));
        Pen { color, layer, width: self.st.widths[n] }
    }

    fn stroke(&mut self, points: Vec<P>) {
        if points.len() < 2 || !self.visible() {
            return;
        }
        let pen = self.pen();
        self.grow(points.iter().copied());
        self.world.path(points, &self.out(), pen);
    }

    fn fill(&mut self, rings: Vec<Vec<P>>, even_odd: bool) {
        if rings.is_empty() || !self.visible() {
            return;
        }
        if matches!(self.st.fill_type, 3 | 4) {
            self.world.skip("Hatch fill (drawn solid)");
        }
        let mut pen = self.pen();
        if self.st.fill_type == 10 && self.st.shade < 100.0 && pen.color != BACKGROUND {
            let rgb = if pen.color == FOREGROUND { 0 } else { pen.color };
            let t = self.st.shade.clamp(0.0, 100.0) / 100.0;
            let mix = |shift: u32| {
                let c = (rgb >> shift & 0xFF) as f64;
                ((255.0 - (255.0 - c) * t).round() as u32) << shift
            };
            pen.color = mix(16) | mix(8) | mix(0);
        }
        self.grow(rings.iter().flatten().copied());
        self.world.fill(rings, even_odd, &self.out(), pen);
    }

    fn flush(&mut self) {
        let line = std::mem::take(&mut self.line);
        self.stroke(line);
    }

    fn move_to(&mut self, p: P) {
        if let Some(rings) = &mut self.st.polygon {
            let ring = rings.last_mut().unwrap();
            if self.st.down {
                ring.push(p);
            } else if ring.len() <= 1 {
                *ring = vec![p];
            } else {
                rings.push(vec![p]);
            }
        } else if self.st.down {
            if self.line.is_empty() {
                self.line.push(self.st.pos);
            }
            self.line.push(p);
        }
        self.st.pos = p;
    }

    fn moves(&mut self, args: &[f64]) {
        for xy in args.chunks_exact(2) {
            let p = if self.st.relative { add(self.st.pos, self.st.vec([xy[0], xy[1]])) } else { self.st.plu([xy[0], xy[1]]) };
            self.move_to(p);
        }
    }

    fn pen_up(&mut self) {
        self.st.down = false;
        self.flush();
    }

    fn select_pen(&mut self, pen: f64) {
        self.flush();
        self.st.pen = (pen.max(0.0) as usize).min(PENS - 1);
    }

    /// Moves along the arc about `centre` (user units) from the pen position, turning by `sweep` radians.
    fn arc_around(&mut self, centre: P, sweep: f64) {
        let start = self.st.user(self.st.pos);
        let r = (start[0] - centre[0]).hypot(start[1] - centre[1]);
        let a0 = (start[1] - centre[1]).atan2(start[0] - centre[0]);
        let n = arc_steps(sweep);
        for k in 1..=n {
            self.move_to(self.st.plu(polar(centre, r, a0 + sweep * k as f64 / n as f64)));
        }
    }

    /// Moves along the arc from the pen position through `mid` to `end` (user units).
    fn arc_through(&mut self, mid: P, end: P) {
        let p0 = self.st.user(self.st.pos);
        let [(x0, y0), (x1, y1), (x2, y2)] = [p0, mid, end].map(|p| (p[0], p[1]));
        let d = 2.0 * (x0 * (y1 - y2) + x1 * (y2 - y0) + x2 * (y0 - y1));
        let size = (x2 - x0).hypot(y2 - y0).max((x1 - x0).hypot(y1 - y0));
        if d.abs() <= 1e-9 * size * size {
            return self.move_to(self.st.plu(end));
        }
        let [s0, s1, s2] = [p0, mid, end].map(|p| p[0] * p[0] + p[1] * p[1]);
        let centre = [(s0 * (y1 - y2) + s1 * (y2 - y0) + s2 * (y0 - y1)) / d, (s0 * (x2 - x1) + s1 * (x0 - x2) + s2 * (x1 - x0)) / d];
        let angle = |p: P| (p[1] - centre[1]).atan2(p[0] - centre[0]);
        let ccw = (angle(end) - angle(p0)).rem_euclid(TAU);
        let sweep = if (angle(mid) - angle(p0)).rem_euclid(TAU) < ccw { ccw } else { ccw - TAU };
        self.arc_around(centre, sweep);
    }

    /// Closed arc points about `centre` (user units), in plotter units.
    fn arc_points(&self, centre: P, r: f64, start: f64, sweep: f64) -> Vec<P> {
        let n = arc_steps(sweep);
        (0..=n).map(|k| self.st.plu(polar(centre, r, start + sweep * k as f64 / n as f64))).collect()
    }

    fn circle(&mut self, r: f64) {
        let ring = self.arc_points(self.st.user(self.st.pos), r, 0.0, TAU);
        let pos = self.st.pos;
        match &mut self.st.polygon {
            Some(rings) => {
                let last = rings.last_mut().unwrap();
                if last.len() <= 1 {
                    *last = ring;
                } else {
                    rings.push(ring);
                }
                rings.push(vec![pos]);
            }
            None => {
                self.flush();
                self.stroke(ring);
            }
        }
    }

    fn rectangle(&mut self, args: &[f64], relative: bool, edge: bool) {
        let (&[x, y, ..], None) = (args, &self.st.polygon) else { return };
        let [x0, y0] = self.st.pos;
        let [x1, y1] = if relative { add(self.st.pos, self.st.vec([x, y])) } else { self.st.plu([x, y]) };
        let ring = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        self.flush();
        if edge { self.stroke(closed(ring)) } else { self.fill(vec![ring], true) }
    }

    fn wedge(&mut self, args: &[f64], edge: bool) {
        let (&[r, start, sweep, ..], None) = (args, &self.st.polygon) else { return };
        let (start, sweep) = (start.to_radians(), sweep.to_radians());
        let mut ring = self.arc_points(self.st.user(self.st.pos), r, start, sweep);
        if sweep.abs() < TAU {
            ring.insert(0, self.st.pos);
        }
        self.flush();
        if edge { self.stroke(closed(ring)) } else { self.fill(vec![ring], true) }
    }

    fn beziers(&mut self, args: &[f64], relative: bool) {
        for set in args.chunks_exact(6) {
            let p0 = self.st.pos;
            let [c1, c2, p3] = [0, 2, 4].map(|k| {
                let v = [set[k], set[k + 1]];
                if relative { add(p0, self.st.vec(v)) } else { self.st.plu(v) }
            });
            for k in 1..=BEZIER_STEPS {
                let t = k as f64 / BEZIER_STEPS as f64;
                let w = [(1.0 - t).powi(3), 3.0 * (1.0 - t).powi(2) * t, 3.0 * (1.0 - t) * t * t, t * t * t];
                let at = |i: usize| w[0] * p0[i] + w[1] * c1[i] + w[2] * c2[i] + w[3] * p3[i];
                self.move_to([at(0), at(1)]);
            }
        }
    }

    fn polyline_encoded(&mut self, data: &[u8]) {
        let (mut up, mut absolute) = (false, false);
        for item in lex::decode_pe(data) {
            match item {
                Pe::Pen(n) => self.select_pen(n as f64),
                Pe::Up => up = true,
                Pe::Absolute => absolute = true,
                Pe::Point(x, y) => {
                    let p = if absolute { self.st.plu([x, y]) } else { add(self.st.pos, self.st.vec([x, y])) };
                    if up {
                        self.pen_up();
                    } else {
                        self.st.down = true;
                    }
                    self.move_to(p);
                    (up, absolute) = (false, false);
                }
            }
        }
    }

    fn label(&mut self, text: &[u8]) {
        self.flush();
        // Latin-1; carriage returns and other control characters are dropped, line feeds start a new line.
        let text: String = text.iter().filter(|&&b| b == b'\n' || (b >= b' ' && b != 0x7F)).map(|&b| b as char).collect();
        let lines: Vec<&str> = text.split('\n').collect();
        let [spacing, pitch] = self.st.text_pitch();
        let h = self.st.char_size[1];
        let (sin, cos) = self.st.direction.sin_cos();
        let lo = self.st.origin % 10;
        let lo = if (1..=9).contains(&lo) { lo - 1 } else { 0 };
        let align = ((lo / 3) as u8, [0, 2, 3][lo as usize % 3]);
        let start = self.st.pos;
        let line_start = |i: usize| add(start, [sin * pitch * i as f64, -cos * pitch * i as f64]);
        if self.st.polygon.is_none() && h > 0.0 && self.visible() {
            let pen = self.pen();
            let factor = spacing / (ADVANCE_PER_CAP * h);
            for (i, line) in lines.iter().enumerate() {
                let pos = line_start(i);
                self.grow([pos]);
                self.world.push_text(pos, self.st.direction, h, factor.clamp(0.2, 5.0), align, line.to_string(), &self.out(), pen);
            }
        }
        // Left-aligned labels leave the pen after the last character; others at the label origin.
        if align.0 == 0 {
            let chars = lines.last().map_or(0, |l| l.chars().count()) as f64;
            self.st.pos = add(line_start(lines.len() - 1), [cos * spacing * chars, sin * spacing * chars]);
        }
    }

    fn command(&mut self, c: Command) {
        self.hpgl = true;
        let a = &c.args[..];
        let arg = |i: usize, default: f64| a.get(i).copied().unwrap_or(default);
        match &c.op {
            b"IN" => {
                self.flush();
                self.st = Plotter::new(self.plot_size);
            }
            b"DF" => {
                self.flush();
                self.st.defaults();
            }
            b"PG" => {
                self.flush();
                if self.drawn {
                    self.page += 1;
                    self.drawn = false;
                }
            }
            b"PS" => {
                if let &[length, width, ..] = a {
                    self.plot_size = Some([length, width]);
                }
            }
            b"IP" | b"IR" => {
                let page = self.plot_size.unwrap_or(P2);
                let point = |i: usize| if c.op == *b"IR" { [a[i] / 100.0 * page[0], a[i + 1] / 100.0 * page[1]] } else { [a[i], a[i + 1]] };
                let size = self.st.span();
                match a.len() {
                    0 | 1 => (self.st.p1, self.st.p2) = self.plot_size.map_or((P1, P2), |s| ([0.0; 2], s)),
                    2 | 3 => (self.st.p1, self.st.p2) = (point(0), add(point(0), size)),
                    _ => (self.st.p1, self.st.p2) = (point(0), point(2)),
                }
                self.st.rescale();
            }
            b"SC" => {
                self.st.sc = a.to_vec();
                self.st.rescale();
            }
            b"RO" => {
                self.flush();
                self.st.rotation = arg(0, 0.0).to_radians();
            }
            b"PU" => {
                self.pen_up();
                self.moves(a);
            }
            b"PD" => {
                self.st.down = true;
                self.moves(a);
            }
            b"PA" | b"PR" => {
                self.st.relative = c.op == *b"PR";
                self.moves(a);
            }
            b"PE" => self.polyline_encoded(&c.text),
            b"AA" | b"AR" if a.len() >= 3 => {
                let centre = if c.op == *b"AR" { add(self.st.user(self.st.pos), [a[0], a[1]]) } else { [a[0], a[1]] };
                self.arc_around(centre, a[2].to_radians());
            }
            b"AT" | b"RT" if a.len() >= 4 => {
                let base = if c.op == *b"RT" { self.st.user(self.st.pos) } else { [0.0; 2] };
                self.arc_through(add(base, [a[0], a[1]]), add(base, [a[2], a[3]]));
            }
            b"CI" if !a.is_empty() => self.circle(a[0]),
            b"EA" | b"ER" | b"RA" | b"RR" => self.rectangle(a, c.op[1] == b'R', c.op[0] == b'E'),
            b"EW" | b"WG" => self.wedge(a, c.op == *b"EW"),
            b"BZ" | b"BR" => self.beziers(a, c.op == *b"BR"),
            b"PM" => match arg(0, 0.0) as i32 {
                0 => {
                    self.flush();
                    self.st.polygon = Some(vec![vec![self.st.pos]]);
                }
                1 => {
                    let pos = self.st.pos;
                    if let Some(rings) = &mut self.st.polygon {
                        rings.push(vec![pos]);
                    }
                }
                2 => {
                    if let Some(rings) = self.st.polygon.take() {
                        self.st.buffer = rings.into_iter().filter(|r| r.len() >= 2).collect();
                    }
                }
                _ => {}
            },
            b"EP" => {
                for ring in self.st.buffer.clone() {
                    self.stroke(closed(ring));
                }
            }
            b"FP" => self.fill(self.st.buffer.clone(), arg(0, 0.0) != 1.0),
            b"FT" => {
                self.st.fill_type = arg(0, 1.0) as u32;
                self.st.shade = if self.st.fill_type == 10 { arg(1, 100.0) } else { 100.0 };
            }
            b"LT" if !a.is_empty() && self.page == self.want => self.world.skip("Line type (drawn solid)"),
            b"IW" if a.len() >= 4 && self.page == self.want => self.world.skip("Clip window (IW)"),
            b"SP" => self.select_pen(arg(0, 0.0)),
            b"NP" => {
                self.flush();
                self.st.colors = (0..PENS).map(default_color).collect();
            }
            b"PC" => {
                self.flush();
                match a {
                    [] => self.st.colors = (0..PENS).map(default_color).collect(),
                    [pen, rest @ ..] => {
                        let pen = (pen.max(0.0) as usize).min(PENS - 1);
                        self.st.colors[pen] = match rest {
                            &[r, g, b, ..] => pen_color(self.st.level(0, r) << 16 | self.st.level(1, g) << 8 | self.st.level(2, b)),
                            _ => default_color(pen),
                        };
                    }
                }
            }
            b"CR" => {
                self.st.range = match a {
                    &[r0, r1, g0, g1, b0, b1, ..] => [[r0, r1], [g0, g1], [b0, b1]],
                    _ => [[0.0, 255.0]; 3],
                };
            }
            b"PW" => {
                self.flush();
                let diagonal = self.st.span()[0].hypot(self.st.span()[1]) * MM_PER_PLU;
                let width = match a.first() {
                    Some(&w) if self.st.relative_widths => w / 100.0 * diagonal,
                    Some(&w) => w,
                    None => PEN_WIDTH,
                };
                match a.get(1) {
                    Some(&pen) => self.st.widths[(pen.max(0.0) as usize).min(PENS - 1)] = width,
                    None => self.st.widths.fill(width),
                }
            }
            b"WU" => {
                self.flush();
                self.st.relative_widths = arg(0, 0.0) == 1.0;
                self.st.widths.fill(PEN_WIDTH);
            }
            b"SI" => self.st.char_size = (if a.len() >= 2 { [a[0], a[1]] } else { CHAR_SIZE }).map(|v| v.abs() * PLU_PER_CM),
            b"SR" => {
                let size = if a.len() >= 2 { [a[0], a[1]] } else { CHAR_SIZE_RELATIVE };
                let span = self.st.span();
                self.st.char_size = [0, 1].map(|k| (size[k] / 100.0 * span[k]).abs());
            }
            b"SD" => {
                let get = |kind: f64| a.chunks_exact(2).find(|p| p[0] == kind).map(|p| p[1]).filter(|v| *v > 0.0);
                let h = get(4.0).map_or(CHAR_SIZE[1] * PLU_PER_CM, |pt| pt / 72.0 * PLU_PER_INCH * CAP_PER_EM);
                let w = get(3.0).map_or(ADVANCE_PER_CAP * h, |pitch| PLU_PER_INCH / pitch) / 1.5;
                self.st.char_size = [w, h];
            }
            b"DI" | b"DR" => {
                let scale = if c.op == *b"DR" { self.st.span().map(|v| v / 100.0) } else { [1.0; 2] };
                let (run, rise) = (arg(0, 1.0) * scale[0], arg(1, 0.0) * scale[1]);
                self.st.direction = if run == 0.0 && rise == 0.0 { 0.0 } else { rise.atan2(run) };
            }
            b"LO" => self.st.origin = arg(0, 1.0) as u32,
            b"CP" => {
                let [spacing, pitch] = self.st.text_pitch();
                let (spaces, lines) = if a.is_empty() { (0.0, -1.0) } else { (arg(0, 0.0), arg(1, 0.0)) };
                let (sin, cos) = self.st.direction.sin_cos();
                let d = [cos * spaces * spacing - sin * lines * pitch, sin * spaces * spacing + cos * lines * pitch];
                self.st.pos = add(self.st.pos, d);
            }
            b"LB" => self.label(&c.text),
            _ => {}
        }
        if matches!(&c.op, b"BP" | b"PS" | b"NP" | b"PC" | b"PW" | b"WU" | b"PE" | b"BZ" | b"BR" | b"CR" | b"SD" | b"FP") {
            self.hpgl2 = true;
        }
    }

    fn info(&self, wrapped: bool) -> Vec<(String, String)> {
        let vector = if self.hpgl2 || wrapped { "HP-GL/2" } else { "HP-GL/1" };
        let mut language = match (self.hpgl, self.images.is_empty()) {
            (false, _) => "HP RTL".to_string(),
            (true, true) => vector.to_string(),
            (true, false) => format!("{vector} with RTL raster"),
        };
        if wrapped {
            language += " (PCL/PJL)";
        }
        let mut info = vec![("Language".to_string(), language)];
        if let Some([length, width]) = self.plot_size {
            info.push(("Plot size".to_string(), format!("{} × {} mm", length * MM_PER_PLU, width * MM_PER_PLU)));
        }
        if !self.pens.is_empty() {
            let list: Vec<String> = self.pens.keys().map(usize::to_string).collect();
            info.push(("Pens".to_string(), list.join(", ")));
        }
        if !self.images.is_empty() {
            let list: Vec<String> = self.images.iter().map(|i| {
                let (w, h) = i.image.size();
                format!("{w} × {h} px at {} dpi", i.dpi)
            }).collect();
            info.push(("Raster images".to_string(), list.join("; ")));
        }
        info
    }

    fn place_images(&mut self) {
        if self.images.is_empty() {
            return;
        }
        let top = match self.plot_size {
            Some([_, width]) => width * MM_PER_PLU,
            None if self.top.is_finite() => self.top,
            None => 0.0,
        };
        let layer = self.world.layer("Raster", FOREGROUND);
        for i in std::mem::take(&mut self.images) {
            self.world.image(i.image, [i.x * MM_PER_INCH, top - i.y * MM_PER_INCH], MM_PER_INCH / i.dpi, layer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(s: &[u8]) -> dxf::Drawing {
        super::decode(s, 0).unwrap().0
    }

    fn close(a: P, b: P) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn lines_in_mm() {
        let d = decode(b"IN;SP1;PU0,0;PD400,0,400,400;PU;SP2;PA0,400;PD;PR0,-400;");
        assert_eq!((d.width, d.height, d.units), (10.0, 10.0, "mm"));
        assert_eq!(d.paths.len(), 2);
        // Output is Y down from the top-left of the extents.
        assert_eq!(d.paths[0].points, [[0.0, 10.0], [10.0, 10.0], [10.0, 0.0]]);
        assert_eq!(d.paths[0].width, PEN_WIDTH);
        assert_eq!(d.paths[1].color, 0xFF0000);
        let names: Vec<_> = d.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Pen 1", "Pen 2"]);
    }

    #[test]
    fn scaling() {
        // User units 0–10 over P1..P2 = 0..4000 plu (100 mm): 1 unit = 10 mm.
        let d = decode(b"IN;IP0,0,4000,4000;SC0,10,0,10;PU0,0;PD10,10;");
        assert!(close(d.paths[0].points[1], [100.0, 0.0]));
        // Point factor: 1 user unit = 2 plu.
        let d = decode(b"IN;IP0,0;SC0,2,0,2,2;PU0,0;PD200,0;");
        assert!((d.width - 10.0).abs() < 1e-9);
    }

    #[test]
    fn arcs_and_circles() {
        let d = decode(b"IN;PU400,0;PD;AA0,0,90;PU;");
        let pts = &d.paths[0].points;
        // Quarter circle of radius 10 mm from (10, 0) to (0, 10).
        assert_eq!(pts.len(), 91);
        assert!(close(pts[90], [0.0, 0.0]));
        let d = decode(b"IN;PU0,0;PD;AT400,400,800,0;");
        assert!((d.height - 10.0).abs() < 1e-6 && (d.width - 20.0).abs() < 1e-6);
        let d = decode(b"IN;PU400,400;CI400;");
        assert!((d.width - 20.0).abs() < 1e-6);
    }

    #[test]
    fn polygons_and_fills() {
        let d = decode(b"IN;PU0,0;PM0;PD400,0,400,400,0,400;PM1;PU100,100;PD200,100,200,200;PM2;FP;EP;FT10,50;RA800,800;");
        assert_eq!(d.fills.len(), 2);
        assert_eq!(d.fills[0].rings.len(), 2);
        assert!(d.fills[0].even_odd);
        assert_eq!(d.paths.len(), 2);
        // 50% shading of black.
        assert_eq!(d.fills[1].color, 0x808080);
        let d = decode(b"IN;PU400,400;WG400,0,90;");
        assert_eq!(d.fills[0].rings[0].len(), 92);
    }

    #[test]
    fn labels() {
        let d = decode(b"IN;SI0.2,0.3;DI0,1;LO5;PU400,400;LBAB\r\nC\x03");
        assert_eq!(d.texts.len(), 2);
        let t = &d.texts[0];
        assert_eq!((t.text.as_str(), t.halign, t.valign), ("AB", 1, 2));
        // Upward text: the baseline runs up the page (negative Y in output); cap height 3 mm.
        assert!(t.x[0].abs() < 1e-9 && t.x[1] < 0.0);
        assert!((t.up[0] + 3.0).abs() < 1e-9);
    }

    #[test]
    fn pages_and_colours() {
        let data = b"IN;PC2,0,0,255;SP2;PD100,0;PG;SP0;PD0,100;PG;";
        let (d, pages) = super::decode(data, 0).unwrap();
        assert_eq!(pages, 2);
        assert_eq!(d.paths[0].color, 0x0000FF);
        let (d, _) = super::decode(data, 1).unwrap();
        assert_eq!(d.paths[0].color, BACKGROUND);
        assert!(super::decode(data, 2).is_err());
    }

    #[test]
    fn widths() {
        let d = decode(b"IN;PW1;PW0.5,1;SP1;PD100,0;SP2;PD200,0;");
        let w: Vec<_> = d.paths.iter().map(|p| p.width).collect();
        assert_eq!(w, [0.5, 1.0]);
    }

    #[test]
    fn raster_on_page() {
        // PS gives the page height; a 1-bit image 8 px wide, 100 dpi, at 1 inch from the top.
        let data = b"\x1bE\x1b%0BIN;PS4000,4000;SP1;PU0,0;PD400,0;\x1b%0A\x1b*t100R\x1b*p0x100Y\x1b*r1A\x1b*b1W\xFF\x1b*rC";
        let (d, _) = super::decode(data, 0).unwrap();
        let [image] = &d.images[..] else { panic!("one image") };
        assert!((image.px - 0.254).abs() < 1e-12);
        // Image top at 100 − 25.4 mm is the top of the extents; the line is at Y 0.
        assert!((d.height - 74.6).abs() < 1e-9, "height {}", d.height);
        assert!(close(image.pos, [0.0, 0.0]));
        assert!(d.info.iter().any(|(k, v)| k == "Language" && v == "HP-GL/2 with RTL raster (PCL/PJL)"));
    }
}
