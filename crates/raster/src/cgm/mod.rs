//! CGM (ISO 8632) metafiles in the binary encoding, drawn through the DXF output helpers.
//!
//! Each picture is a page. Virtual device coordinates (VDC) map to world coordinates by the
//! direction of the VDC extent (its first corner is the bottom-left of the picture) and, in metric
//! scaling mode, the millimetres per VDC unit. Filled areas are drawn under all lines.

mod read;

use std::f64::consts::TAU;

use crate::dwf::image::{self as raster_image, Kind};
use crate::dxf::{self, Affine, Arc, Pen, World, BACKGROUND, FOREGROUND, P};
use crate::rgba::{rgb, RgbaImage, MAX_PIXELS};
use crate::tiff::Image;
use read::{Bits, Command, Params, Precision, Real, R};

/// Segments per full turn when an arc bounds a filled area.
const ARC_STEPS: f64 = 72.0;
const BEZIER_STEPS: usize = 32;
/// Scaled line widths are multiples of this fraction of the VDC extent's longer side. The standard
/// leaves the nominal width to the device; a scale of 1 or less is drawn as a hairline.
const NOMINAL_WIDTH: f64 = 1.0 / 1000.0;

/// Decodes picture `page` (0-based) and returns it with the picture count.
pub fn decode(data: &[u8], page: u32) -> Result<(dxf::Drawing, u32), String> {
    let text = data.trim_ascii_start();
    if text.len() >= 5 && text[..5].eq_ignore_ascii_case(b"BEGMF") {
        return Err("Clear-text CGM files are not supported, only the binary encoding".into());
    }
    if !(data.len() >= 2 && data[0] == 0 && data[1] & 0xE0 == 0x20) {
        return Err("Not a binary CGM file".into());
    }
    let mut d = Decoder::new(page);
    for c in read::commands(data) {
        let c = match c {
            Ok(c) if (c.class, c.id) == (0, 2) => break,
            Ok(c) => c,
            Err(e) => {
                d.warning.get_or_insert(e);
                break;
            }
        };
        if let Err(e) = d.command(&c, false) {
            d.warning.get_or_insert(format!("{} ({e})", element_name(c.class, c.id)));
        }
    }
    if d.pictures == 0 {
        return Err("The CGM file has no pictures".into());
    }
    if page >= d.pictures {
        return Err(format!("Page {} does not exist", page + 1));
    }
    let mut info = d.info;
    info.extend(d.warning.map(|w| ("Warning".to_string(), w)));
    let units = if d.metric.is_some() { "mm" } else { "" };
    info.push(("Scale".into(), d.metric.map_or("Abstract (no physical size)".into(), |f| format!("1 VDC unit = {f} mm"))));
    d.world.finish(units, info).map(|drawing| (drawing, d.pictures))
}

fn element_name(class: u8, id: u8) -> String {
    format!("Element {class}/{id}")
}

#[derive(Clone, Copy)]
enum Colour {
    Index(u32),
    /// 0xRRGGBB, `FOREGROUND` or `BACKGROUND`.
    Direct(u32),
}

/// Black and white become the drawing's foreground and paper, so they swap when the view is inverted.
fn ink(c: u32) -> u32 {
    match c {
        0 => FOREGROUND,
        0xFFFFFF => BACKGROUND,
        c => c,
    }
}

/// The reverse of `ink`, for image pixels.
fn plain(c: u32) -> u32 {
    match c {
        FOREGROUND => 0,
        BACKGROUND => 0xFFFFFF,
        c => c,
    }
}

fn pixel(c: u32) -> u32 {
    let c = plain(c);
    rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Picture state, reset to the metafile defaults at each BEGIN PICTURE.
#[derive(Clone)]
struct State {
    prec: Precision,
    color_min: [f64; 4],
    color_max: [f64; 4],
    direct: bool,
    table: Vec<u32>,
    /// Millimetres per VDC unit in metric scaling mode.
    metric: Option<f64>,
    /// Width specification modes: 0 absolute (VDC), 1 scaled, 2 fractional, 3 millimetres.
    line_mode: i64,
    edge_mode: i64,
    extent: Option<[P; 2]>,
    /// `None` until set: the nominal width.
    line_width: Option<f64>,
    line_color: Colour,
    edge_width: Option<f64>,
    edge_color: Colour,
    edge_visible: bool,
    /// 0 hollow, 1 solid, 2 pattern, 3 hatch, 4 empty, 5 geometric pattern, 6 interpolated.
    interior: i64,
    fill_color: Colour,
    text_color: Colour,
    char_height: Option<f64>,
    char_up: P,
    char_base: P,
    expansion: f64,
    align: (u8, u8),
}

impl Default for State {
    fn default() -> Self {
        let black = Colour::Index(1);
        State {
            prec: Precision::default(),
            color_min: [0.0; 4],
            color_max: [255.0; 4],
            direct: false,
            table: vec![BACKGROUND, FOREGROUND],
            metric: None,
            line_mode: 1,
            edge_mode: 1,
            extent: None,
            line_width: None,
            line_color: black,
            edge_width: None,
            edge_color: black,
            edge_visible: false,
            interior: 0,
            fill_color: black,
            text_color: black,
            char_height: None,
            char_up: [0.0, 1.0],
            char_base: [1.0, 0.0],
            expansion: 1.0,
            align: (0, 0),
        }
    }
}

impl State {
    fn extent(&self) -> [P; 2] {
        self.extent.unwrap_or(if self.prec.vdc_real { [[0.0; 2], [1.0; 2]] } else { [[0.0; 2], [32767.0; 2]] })
    }

    fn longest_side(&self) -> f64 {
        let [a, b] = self.extent();
        (b[0] - a[0]).abs().max((b[1] - a[1]).abs())
    }

    /// VDC to world: flips axes that run against the extent and scales to millimetres.
    fn m(&self) -> Affine {
        let [a, b] = self.extent();
        let f = self.metric.unwrap_or(1.0);
        let sign = |lo: f64, hi: f64| if hi < lo { -f } else { f };
        Affine::scale(sign(a[0], b[0]), sign(a[1], b[1]))
    }

    /// Line or edge width in world units (0 is a hairline).
    fn width(&self, mode: i64, value: Option<f64>) -> f64 {
        let Some(w) = value else { return 0.0 };
        let vdc = match mode {
            0 => w,
            1 if w <= 1.0 => 0.0,
            1 => w * self.longest_side() * NOMINAL_WIDTH,
            2 => {
                let [a, b] = self.extent();
                w * (b[0] - a[0])
            }
            // Millimetres are world units in metric mode; otherwise there is no scale, so take them as VDC.
            _ => return w.abs(),
        };
        vdc.abs() * self.metric.unwrap_or(1.0)
    }

    fn color(&self, r: &mut Params) -> R<Colour> {
        Ok(if self.direct { Colour::Direct(self.rgb(r.color_direct()?)) } else { Colour::Index(r.color_index()?) })
    }

    /// Direct colour components, scaled by the colour value extent.
    fn rgb(&self, c: [u32; 4]) -> u32 {
        let n = |k: usize| ((c[k] as f64 - self.color_min[k]) / (self.color_max[k] - self.color_min[k]).max(1e-9)).clamp(0.0, 1.0);
        let [r, g, b] = if self.prec.components == 4 {
            let k = 1.0 - n(3);
            [(1.0 - n(0)) * k, (1.0 - n(1)) * k, (1.0 - n(2)) * k]
        } else {
            [n(0), n(1), n(2)]
        };
        let byte = |v: f64| (v * 255.0).round() as u32;
        ink(byte(r) << 16 | byte(g) << 8 | byte(b))
    }

    fn resolve(&self, c: Colour) -> u32 {
        match c {
            Colour::Index(i) => self.table.get(i as usize).copied().unwrap_or(FOREGROUND),
            Colour::Direct(c) => c,
        }
    }
}

/// An application structure; WebCGM layers are those of type "layer".
struct Aps {
    kind: String,
    layer: Option<u32>,
}

/// The open tile array: where tile `next` goes.
struct Tiles {
    pos: P,
    /// One tile's extent along the cell path and the line progression, in VDC.
    path: P,
    line: P,
    cells: (u32, u32),
    across: u32,
    next: u32,
}

struct Decoder {
    page: u32,
    pictures: u32,
    /// Inside the requested picture.
    active: bool,
    st: State,
    /// The state after the metafile descriptor, which each picture starts from.
    defaults: Option<State>,
    world: World<'static>,
    info: Vec<(String, String)>,
    warning: Option<String>,
    /// Millimetres per VDC unit of the requested picture, if metric.
    metric: Option<f64>,
    aps: Vec<Aps>,
    default_layer: Option<u32>,
    /// Text waiting for APPEND TEXT: position and characters so far.
    text: Option<(P, String)>,
    tiles: Option<Tiles>,
}

impl Decoder {
    fn new(page: u32) -> Self {
        Decoder {
            page,
            pictures: 0,
            active: false,
            st: State::default(),
            defaults: None,
            world: World::new(Vec::new()),
            info: Vec::new(),
            warning: None,
            metric: None,
            aps: Vec::new(),
            default_layer: None,
            text: None,
            tiles: None,
        }
    }

    /// `force` applies picture elements outside the requested picture (in METAFILE DEFAULTS REPLACEMENT).
    fn command(&mut self, c: &Command, force: bool) -> R<()> {
        let prec = self.st.prec.clone();
        let r = &mut read::params(&c.data, &prec);
        match c.class {
            0 => self.delimiter(c.id, r),
            1 => self.descriptor(c.id, r),
            _ if !self.active && !force => Ok(()),
            2 => self.picture_descriptor(c.id, r),
            3 => self.control(c.id, r),
            4 => self.primitive(c.id, r),
            5 => self.attribute(c.id, r),
            9 => self.aps_attribute(c.id, r),
            _ => Ok(()),
        }
    }

    fn delimiter(&mut self, id: u8, r: &mut Params) -> R<()> {
        match id {
            // BEGIN PICTURE
            3 => {
                self.st = self.defaults.get_or_insert_with(|| self.st.clone()).clone();
                self.active = self.pictures == self.page;
                self.pictures += 1;
                (self.aps, self.text, self.tiles) = (Vec::new(), None, None);
                if self.active {
                    self.metric = self.st.metric;
                    let name = r.string().unwrap_or_default();
                    if !name.trim().is_empty() {
                        self.info.push(("Picture".into(), name));
                    }
                }
            }
            // END PICTURE
            5 => self.active = false,
            _ if !self.active => {}
            // BEGIN FIGURE: its parts are drawn as separate shapes.
            8 => self.world.skip("Figure (parts drawn separately)"),
            // BEGIN TILE ARRAY
            19 => {
                let pos = r.point()?;
                let (path_dir, line_dir) = (r.enumeration()?, r.enumeration()?);
                let (across, _down) = (r.int()?, r.int()?);
                let cells = (r.int()?, r.int()?);
                let size = (r.real()?, r.real()?);
                if cells.0 <= 0 || cells.1 <= 0 || across <= 0 {
                    return Err("bad tile array size".into());
                }
                // Cell path at 0°, 90°, 180° or 270°; line progression 90° or 270° from it.
                let path = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]][(path_dir & 3) as usize];
                let line = if line_dir == 0 { [-path[1], path[0]] } else { [path[1], -path[0]] };
                let scaled = |v: P, len: f64| [v[0] * len, v[1] * len];
                self.tiles = Some(Tiles {
                    pos,
                    path: scaled(path, cells.0 as f64 * size.0),
                    line: scaled(line, cells.1 as f64 * size.1),
                    cells: (cells.0 as u32, cells.1 as u32),
                    across: across as u32,
                    next: 0,
                });
            }
            // END TILE ARRAY
            20 => self.tiles = None,
            // BEGIN APPLICATION STRUCTURE
            21 => {
                let _id = r.string()?;
                let kind = r.string()?.to_lowercase();
                self.aps.push(Aps { kind, layer: None });
            }
            // END APPLICATION STRUCTURE
            23 => {
                self.aps.pop();
            }
            _ => {}
        }
        Ok(())
    }

    fn descriptor(&mut self, id: u8, r: &mut Params) -> R<()> {
        let p = &mut self.st.prec;
        match id {
            1 => self.info.push(("CGM version".into(), r.int()?.to_string())),
            2 => self.info.push(("Description".into(), r.string()?)),
            3 => p.vdc_real = r.enumeration()? == 1,
            4 => p.int = read::bytes(r.int()?)?,
            5 => p.real = Real::new(r.enumeration()?, r.int()?, r.int()?)?,
            6 => p.index = read::bytes(r.int()?)?,
            7 => p.color_bits = read::bytes(r.int()?)? * 8,
            8 => p.color_index_bits = read::bytes(r.int()?)? * 8,
            10 => {
                let (lo, hi) = (r.color_direct()?, r.color_direct()?);
                self.st.color_min = lo.map(|v| v as f64);
                self.st.color_max = hi.map(|v| v as f64);
            }
            // METAFILE DEFAULTS REPLACEMENT: elements that change the picture defaults.
            12 => {
                for c in read::commands(r.rest()) {
                    self.command(&c?, true)?;
                }
            }
            13 => {
                let mut fonts = Vec::new();
                while !r.is_empty() {
                    fonts.push(r.string()?);
                }
                self.info.push(("Fonts".into(), fonts.join(", ")));
            }
            19 => {
                p.components = match r.index()? {
                    1 => 3,
                    6 => 4,
                    m => return Err(format!("colour model {m} is not supported")),
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn picture_descriptor(&mut self, id: u8, r: &mut Params) -> R<()> {
        match id {
            1 => {
                let metric = r.enumeration()? == 1;
                let f = scale_factor(r)?;
                self.st.metric = (metric && f > 0.0).then_some(f);
                if self.active {
                    self.metric = self.st.metric;
                }
            }
            2 => self.st.direct = r.enumeration()? == 1,
            3 => self.st.line_mode = r.enumeration()?,
            5 => self.st.edge_mode = r.enumeration()?,
            6 => self.st.extent = Some([r.point()?, r.point()?]),
            _ => {}
        }
        Ok(())
    }

    fn control(&mut self, id: u8, r: &mut Params) -> R<()> {
        let p = &mut self.st.prec;
        match id {
            1 => p.vdc_int = read::bytes(r.int()?)?,
            2 => p.vdc_float = Real::new(r.enumeration()?, r.int()?, r.int()?)?,
            _ => {}
        }
        Ok(())
    }

    fn attribute(&mut self, id: u8, r: &mut Params) -> R<()> {
        let st = &mut self.st;
        let width = |r: &mut Params, mode: i64| if mode == 0 { r.vdc() } else { r.real() };
        match id {
            2 | 27 if r.index()? != 1 => self.world.skip("Dashed lines (drawn solid)"),
            3 => st.line_width = Some(width(r, st.line_mode)?),
            4 => st.line_color = st.color(r)?,
            12 => st.expansion = r.real()?,
            14 => st.text_color = st.color(r)?,
            15 => st.char_height = Some(r.vdc()?),
            16 => (st.char_up, st.char_base) = (r.point()?, r.point()?),
            17 if r.enumeration()? != 0 => self.world.skip("Vertical or reversed text (drawn left to right)"),
            18 => {
                let h = match r.enumeration()? {
                    2 => 1,
                    3 => 2,
                    _ => 0,
                };
                let v = match r.enumeration()? {
                    1 | 2 => 3,
                    3 => 2,
                    5 => 1,
                    _ => 0,
                };
                st.align = (h, v);
            }
            22 => st.interior = r.enumeration()?,
            23 => st.fill_color = st.color(r)?,
            28 => st.edge_width = Some(width(r, st.edge_mode)?),
            29 => st.edge_color = st.color(r)?,
            30 => st.edge_visible = r.enumeration()? == 1,
            34 => {
                let start = r.color_index()? as usize;
                let mut k = start;
                while !r.is_empty() {
                    let c = st.rgb(r.color_direct()?);
                    if st.table.len() <= k {
                        st.table.resize(k + 1, FOREGROUND);
                    }
                    st.table[k] = c;
                    k += 1;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn aps_attribute(&mut self, id: u8, r: &mut Params) -> R<()> {
        if id != 1 || self.aps.last().is_none_or(|a| a.kind != "layer") {
            return Ok(());
        }
        if r.string()?.eq_ignore_ascii_case("layername") {
            if let Some(name) = r.sdr_string()? {
                let layer = self.world.layer(name.trim(), FOREGROUND);
                self.aps.last_mut().unwrap().layer = Some(layer);
            }
        }
        Ok(())
    }

    fn layer(&mut self) -> u32 {
        if let Some(layer) = self.aps.iter().rev().find_map(|a| a.layer) {
            return layer;
        }
        *self.default_layer.get_or_insert_with(|| self.world.layer("Drawing", FOREGROUND))
    }

    fn pen(&mut self, color: Colour, width: f64) -> Pen {
        Pen { color: self.st.resolve(color), layer: self.layer(), width }
    }

    fn line_pen(&mut self) -> Pen {
        self.pen(self.st.line_color, self.st.width(self.st.line_mode, self.st.line_width))
    }

    fn primitive(&mut self, id: u8, r: &mut Params) -> R<()> {
        let m = self.st.m();
        match id {
            // POLYLINE
            1 => {
                let pen = self.line_pen();
                self.world.path(r.points()?, &m, pen);
            }
            // DISJOINT POLYLINE
            2 => {
                let pen = self.line_pen();
                for pair in r.points()?.chunks_exact(2) {
                    self.world.path(pair.to_vec(), &m, pen);
                }
            }
            // TEXT, RESTRICTED TEXT (its box is ignored) and APPEND TEXT
            4 | 5 => {
                if id == 5 {
                    let _box = (r.vdc()?, r.vdc()?);
                }
                let pos = r.point()?;
                let last = r.enumeration()? == 1;
                self.text = Some((pos, r.string()?));
                if last {
                    self.flush_text();
                }
            }
            6 => {
                let last = r.enumeration()? == 1;
                let s = r.string()?;
                if let Some((_, text)) = &mut self.text {
                    text.push_str(&s);
                }
                if last {
                    self.flush_text();
                }
            }
            // POLYGON
            7 => {
                let points = r.points()?;
                let mut closed = points.clone();
                closed.extend(points.first().copied());
                self.area(vec![points], |w, m, pen, _| w.path(closed.clone(), m, pen));
            }
            8 => self.polygon_set(r)?,
            9 => self.cell_array(r)?,
            // RECTANGLE
            11 => {
                let ([x0, y0], [x1, y1]) = (r.point()?, r.point()?);
                let ring = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
                let closed = [&ring[..], &ring[..1]].concat();
                self.area(vec![ring], |w, m, pen, _| w.path(closed.clone(), m, pen));
            }
            // CIRCLE
            12 => {
                let (c, radius) = (r.point()?, r.vdc()?);
                self.arc(c, [radius, 0.0], [0.0, radius], 0.0, TAU, Some(-1));
            }
            // CIRCULAR ARC 3 POINT (CLOSE)
            13 | 14 => {
                let (a, b, c) = (r.point()?, r.point()?, r.point()?);
                let close = if id == 14 { Some(r.enumeration()?) } else { None };
                match three_point_arc(a, b, c) {
                    Some((centre, radius, t0, t1)) => self.arc(centre, [radius, 0.0], [0.0, radius], t0, t1, close),
                    None => {
                        let pen = self.line_pen();
                        self.world.path(vec![a, b, c], &m, pen);
                    }
                }
            }
            // CIRCULAR ARC CENTRE (CLOSE, REVERSED)
            15 | 16 | 20 => {
                let (c, start, end, radius) = (r.point()?, r.point()?, r.point()?, r.vdc()?);
                let close = if id == 16 { Some(r.enumeration()?) } else { None };
                let (t0, t1) = (start[1].atan2(start[0]), end[1].atan2(end[0]));
                let t1 = if id == 20 { t0 - dxf::sweep(t1, t0) } else { t0 + dxf::sweep(t0, t1) };
                self.arc(c, [radius, 0.0], [0.0, radius], t0, t1, close);
            }
            // ELLIPSE
            17 => {
                let (c, a, b) = (r.point()?, r.point()?, r.point()?);
                self.arc(c, sub(a, c), sub(b, c), 0.0, TAU, Some(-1));
            }
            // ELLIPTICAL ARC (CLOSE)
            18 | 19 => {
                let (c, a, b, start, end) = (r.point()?, r.point()?, r.point()?, r.point()?, r.point()?);
                let close = if id == 19 { Some(r.enumeration()?) } else { None };
                let (u, v) = (sub(a, c), sub(b, c));
                let (t0, t1) = (parameter(u, v, start), parameter(u, v, end));
                self.arc(c, u, v, t0, t0 + dxf::sweep(t0, t1), close);
            }
            // POLYBEZIER: separate curves of 4 points, or a continuous one of 4 + 3k.
            26 => {
                let continuous = r.index()? == 2;
                let points = r.points()?;
                let pen = self.line_pen();
                if continuous {
                    let mut path: Vec<P> = points.iter().take(1).copied().collect();
                    for c in points.get(1..).unwrap_or_default().chunks_exact(3) {
                        bezier(&mut path, [c[0], c[1], c[2]]);
                    }
                    self.world.path(path, &m, pen);
                } else {
                    for c in points.chunks_exact(4) {
                        let mut path = vec![c[0]];
                        bezier(&mut path, [c[1], c[2], c[3]]);
                        self.world.path(path, &m, pen);
                    }
                }
            }
            28 | 29 => self.tile(id == 28, r)?,
            3 => self.world.skip("Polymarker"),
            10 => self.world.skip("Generalized drawing primitive"),
            21 => self.world.skip("Connecting edge"),
            22 => self.world.skip("Hyperbolic arc"),
            23 => self.world.skip("Parabolic arc"),
            24 | 25 => self.world.skip("B-spline"),
            27 => self.world.skip("Polysymbol"),
            _ => {}
        }
        Ok(())
    }

    fn flush_text(&mut self) {
        let Some((pos, text)) = self.text.take() else { return };
        let st = &self.st;
        let height = st.char_height.unwrap_or(st.longest_side() / 100.0).abs();
        let unit = |v: P| {
            let len = v[0].hypot(v[1]);
            if len > 0.0 { [v[0] / len, v[1] / len] } else { v }
        };
        let (x, up) = (unit(st.char_base), unit(st.char_up));
        let x = [x[0] * height * st.expansion, x[1] * height * st.expansion];
        let up = [up[0] * height, up[1] * height];
        let (m, align) = (st.m(), st.align);
        let pen = self.pen(self.st.text_color, 0.0);
        self.world.text_frame(pos, x, up, align, text, &m, pen);
    }

    /// A filled-area primitive: `rings` are filled or outlined according to the interior style,
    /// and `edge(world, m, pen, all)` strokes its boundary (`all`: ignore per-edge visibility).
    fn area(&mut self, rings: Vec<Vec<P>>, edge: impl Fn(&mut World<'static>, &Affine, Pen, bool)) {
        let m = self.st.m();
        let fill = self.pen(self.st.fill_color, 0.0);
        match self.st.interior {
            0 => edge(&mut self.world, &m, fill, true),
            3 => {
                self.world.skip("Hatch fill (outline only)");
                edge(&mut self.world, &m, fill, true);
            }
            4 => {}
            style => {
                if style != 1 {
                    self.world.skip("Pattern fill (drawn solid)");
                }
                self.world.fill(rings, true, &m, fill);
            }
        }
        if self.st.edge_visible {
            let pen = self.pen(self.st.edge_color, self.st.width(self.st.edge_mode, self.st.edge_width));
            edge(&mut self.world, &m, pen, false);
        }
    }

    /// The points `c + u·cos t + v·sin t` from `t0` to `t1`: a line if `close` is `None`, else a
    /// filled area closed as a pie (0), a chord (1) or, for -1, a full turn.
    fn arc(&mut self, c: P, u: P, v: P, t0: f64, t1: f64, close: Option<i64>) {
        let m = self.st.m();
        let shape = move |pen: Pen| Arc { centre: c, u, v, t0, t1, color: pen.color, width: pen.width, layer: pen.layer };
        let Some(close) = close else {
            let pen = self.line_pen();
            self.world.arc(shape(pen), &m);
            return;
        };
        let at = |t: f64| [c[0] + u[0] * t.cos() + v[0] * t.sin(), c[1] + u[1] * t.cos() + v[1] * t.sin()];
        let steps = ((t1 - t0).abs() / TAU * ARC_STEPS).ceil().max(4.0) as usize;
        let mut ring: Vec<P> = (0..=steps).map(|k| at(t0 + (t1 - t0) * k as f64 / steps as f64)).collect();
        let (start, end) = (at(t0), at(t1));
        let closing = match close {
            0 => vec![end, c, start],
            1 => vec![end, start],
            _ => Vec::new(),
        };
        if close == 0 {
            ring.push(c);
        }
        self.area(vec![ring], |w, m, pen, _| {
            w.arc(shape(pen), m);
            w.path(closing.clone(), m, pen);
        });
    }

    /// POLYGON SET: points with edge flags (0 invisible, 1 visible, 2 and 3 the same but closing the ring).
    fn polygon_set(&mut self, r: &mut Params) -> R<()> {
        let mut rings: Vec<Vec<(P, bool)>> = vec![Vec::new()];
        while !r.is_empty() {
            let (p, flag) = (r.point()?, r.enumeration()?);
            rings.last_mut().unwrap().push((p, flag & 1 == 1));
            if flag >= 2 {
                rings.push(Vec::new());
            }
        }
        rings.retain(|ring| !ring.is_empty());
        // Edge i leaves vertex i; the last one closes the ring.
        let segments: Vec<(P, P, bool)> =
            rings.iter().flat_map(|ring| (0..ring.len()).map(move |i| (ring[i].0, ring[(i + 1) % ring.len()].0, ring[i].1))).collect();
        let rings = rings.into_iter().map(|ring| ring.into_iter().map(|(p, _)| p).collect()).collect();
        self.area(rings, |w, m, pen, all| {
            let mut run: Vec<P> = Vec::new();
            for &(a, b, _) in segments.iter().filter(|s| s.2 || all) {
                if run.last() != Some(&a) {
                    w.path(std::mem::take(&mut run), m, pen);
                    run.push(a);
                }
                run.push(b);
            }
            w.path(run, m, pen);
        });
        Ok(())
    }

    /// CELL ARRAY: corners P (first cell), Q (opposite) and R (end of the first row), cells in
    /// packed or run-length rows that start on 16-bit boundaries.
    fn cell_array(&mut self, r: &mut Params) -> R<()> {
        let (p, q, corner) = (r.point()?, r.point()?, r.point()?);
        let (nx, ny) = (r.int()?, r.int()?);
        let local = r.int()? as usize;
        let packed = r.enumeration()? == 1;
        let n = nx as u64 * ny as u64;
        if nx <= 0 || ny <= 0 || n > MAX_PIXELS {
            return Err(format!("{nx} × {ny} cell array is empty or too large"));
        }
        let st = &self.st;
        let full = if st.direct { st.prec.color_bits } else { st.prec.color_index_bits };
        let bits = if local == 0 { full } else { local };
        if bits > 32 {
            return Err(format!("{bits}-bit cell colours"));
        }
        let int_bits = st.prec.int * 8;
        let mut b = Bits::new(r.rest());
        // At a local precision the colour value extent does not apply: values span the full range.
        let local_max = ((1u64 << bits) - 1) as f64;
        let rescale = |v: u32, k: usize| {
            if bits == st.prec.color_bits { v } else { (st.color_min[k] + v as f64 / local_max * (st.color_max[k] - st.color_min[k])).round() as u32 }
        };
        let cell = |b: &mut Bits| -> R<u32> {
            Ok(if st.direct {
                let mut c = [0; 4];
                for (k, v) in c.iter_mut().enumerate().take(st.prec.components) {
                    *v = rescale(b.read(bits)?, k);
                }
                st.rgb(c)
            } else {
                st.resolve(Colour::Index(b.read(bits)?))
            })
        };
        let (nx, ny) = (nx as usize, ny as usize);
        let mut data = Vec::with_capacity(nx * ny);
        for row in 1..=ny {
            while data.len() < row * nx {
                let count = if packed { 1 } else { b.read(int_bits)? as usize };
                let c = pixel(cell(&mut b)?);
                if count == 0 {
                    return Err("empty run in cell array".into());
                }
                data.extend(std::iter::repeat_n(c, count.min(row * nx - data.len())));
            }
            b.align();
        }
        let image = Image::Color(RgbaImage { width: nx as u32, height: ny as u32, data });
        self.place(image, p, sub(corner, p), sub(q, corner));
        Ok(())
    }

    /// TILE and BITONAL TILE inside a tile array: T6 (Group 4), JPEG and PNG compression.
    fn tile(&mut self, bitonal: bool, r: &mut Params) -> R<()> {
        let Some(t) = &mut self.tiles else {
            return Err("tile outside a tile array".into());
        };
        let k = t.next;
        t.next += 1;
        let (i, j) = ((k % t.across) as f64, (k / t.across) as f64);
        let pos = [t.pos[0] + i * t.path[0] + j * t.line[0], t.pos[1] + i * t.path[1] + j * t.line[1]];
        let (path, line, (w, h)) = (t.path, t.line, t.cells);
        let compression = r.index()?;
        let _row_padding = r.int()?;
        let (paper, ink_color) = if bitonal {
            let (bg, fg) = (self.st.color(r)?, self.st.color(r)?);
            (plain(self.st.resolve(bg)), plain(self.st.resolve(fg)))
        } else {
            r.int()?;
            (0xFFFFFF, 0)
        };
        r.string_bytes()?;
        let data = r.rest();
        let image = match compression {
            // Null background: nothing to draw.
            0 => return Ok(()),
            2 => raster_image::decode(Kind::Group4Mapped, w, h, &[paper, ink_color], data),
            7 => raster_image::decode(Kind::Jpeg, w, h, &[], data),
            9 => raster_image::decode(Kind::Png, w, h, &[], data),
            _ => {
                self.world.skip("Tile (unsupported compression)");
                return Ok(());
            }
        };
        match image {
            Ok(image) => self.place(image, pos, path, line),
            Err(_) => self.world.skip("Tile (unreadable)"),
        }
        Ok(())
    }

    /// Places `image` with its first pixel at `p`, rows running along `row` and successive rows
    /// along `col` (VDC vectors spanning the whole image). Only axis-aligned placements are drawn.
    fn place(&mut self, image: Image, p: P, row: P, col: P) {
        let m = self.st.m();
        let (row, col) = (m.linear(row), m.linear(col));
        let (Some(pel_path), Some(col_angle)) = (axis(row), axis(col)) else {
            self.world.skip("Rotated raster image");
            return;
        };
        let line_prog = (col_angle + 360 - pel_path) % 360;
        if line_prog % 180 != 90 {
            self.world.skip("Rotated raster image");
            return;
        }
        let image = match image {
            Image::Bilevel(b) => Image::Bilevel(b.oriented(pel_path, line_prog)),
            Image::Color(c) => Image::Color(c.oriented(pel_path, line_prog)),
        };
        let p = m.apply(p);
        let corners = [p, [p[0] + row[0], p[1] + row[1]], [p[0] + col[0], p[1] + col[1]], [p[0] + row[0] + col[0], p[1] + row[1] + col[1]]];
        let left = corners.iter().map(|c| c[0]).fold(f64::INFINITY, f64::min);
        let top = corners.iter().map(|c| c[1]).fold(f64::NEG_INFINITY, f64::max);
        let px = (row[0].abs() + col[0].abs()) / image.size().0 as f64;
        let layer = self.layer();
        self.world.image(image, [left, top], px, layer);
    }
}

/// SCALING MODE's factor is a floating-point real whatever the REAL PRECISION; files written with
/// a fixed-point precision may store it in that format instead, so an implausible float falls back to it.
fn scale_factor(r: &mut Params) -> R<f64> {
    if r.remaining() >= 8 {
        return r.real_as(Real::Float64);
    }
    let bytes = r.bytes(4)?;
    // Through its shortest decimal form, so 0.1 stays 0.1 rather than 0.10000000149.
    let float: f64 = f32::from_be_bytes(bytes.try_into().unwrap()).to_string().parse().unwrap_or(0.0);
    if (1e-9..1e9).contains(&float.abs()) {
        return Ok(float);
    }
    let precision = Precision::default();
    read::params(bytes, &precision).real_as(Real::Fixed32)
}

/// Angle of an axis-aligned vector (counter-clockwise, Y up) in degrees.
fn axis(v: P) -> Option<u32> {
    let len = v[0].hypot(v[1]);
    if len == 0.0 {
        None
    } else if v[1].abs() <= len * 1e-6 {
        Some(if v[0] > 0.0 { 0 } else { 180 })
    } else if v[0].abs() <= len * 1e-6 {
        Some(if v[1] > 0.0 { 90 } else { 270 })
    } else {
        None
    }
}

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}

/// The parameter `t` at which `u·cos t + v·sin t` points along `d`.
fn parameter(u: P, v: P, d: P) -> f64 {
    let det = u[0] * v[1] - u[1] * v[0];
    if det == 0.0 {
        return d[1].atan2(d[0]);
    }
    let (c, s) = ((d[0] * v[1] - d[1] * v[0]) / det, (u[0] * d[1] - u[1] * d[0]) / det);
    s.atan2(c)
}

/// Centre, radius and start/end angles of the arc from `a` through `b` to `c`, or `None` if they are collinear.
fn three_point_arc(a: P, b: P, c: P) -> Option<(P, f64, f64, f64)> {
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    let span = sub(c, a)[0].hypot(sub(c, a)[1]).max(sub(b, a)[0].hypot(sub(b, a)[1]));
    if d.abs() <= 1e-12 * span * span {
        return None;
    }
    let sq = |p: P| p[0] * p[0] + p[1] * p[1];
    let x = (sq(a) * (b[1] - c[1]) + sq(b) * (c[1] - a[1]) + sq(c) * (a[1] - b[1])) / d;
    let y = (sq(a) * (c[0] - b[0]) + sq(b) * (a[0] - c[0]) + sq(c) * (b[0] - a[0])) / d;
    let centre = [x, y];
    let angle = |p: P| (p[1] - y).atan2(p[0] - x);
    let (t0, tb, tc) = (angle(a), angle(b), angle(c));
    let ccw = (tc - t0).rem_euclid(TAU);
    let t1 = if (tb - t0).rem_euclid(TAU) < ccw { t0 + ccw } else { t0 - (TAU - ccw) };
    Some((centre, sub(a, centre)[0].hypot(sub(a, centre)[1]), t0, t1))
}

/// Appends a cubic Bézier from the path's last point through controls `c`.
fn bezier(path: &mut Vec<P>, [c1, c2, p3]: [P; 3]) {
    let Some(&p0) = path.last() else { return };
    for k in 1..=BEZIER_STEPS {
        let t = k as f64 / BEZIER_STEPS as f64;
        let w = [(1.0 - t).powi(3), 3.0 * (1.0 - t).powi(2) * t, 3.0 * (1.0 - t) * t * t, t * t * t];
        let at = |i: usize| w[0] * p0[i] + w[1] * c1[i] + w[2] * c2[i] + w[3] * p3[i];
        path.push([at(0), at(1)]);
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_2, PI};

    use super::*;

    fn cmd(class: u8, id: u8, data: &[u8]) -> Vec<u8> {
        let head = (class as u16) << 12 | (id as u16) << 5;
        let mut out = Vec::new();
        if data.len() < 31 {
            out.extend((head | data.len() as u16).to_be_bytes());
        } else {
            out.extend((head | 31).to_be_bytes());
            out.extend((data.len() as u16).to_be_bytes());
        }
        out.extend(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn ints(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn s(text: &str) -> Vec<u8> {
        [&[text.len() as u8][..], text.as_bytes()].concat()
    }

    fn polyline(points: &[i16]) -> Vec<u8> {
        cmd(4, 1, &ints(points))
    }

    /// A metafile with `descriptor` elements and one picture per entry of `pictures`.
    fn file(descriptor: &[Vec<u8>], pictures: &[&[Vec<u8>]]) -> Vec<u8> {
        let mut out = cmd(0, 1, &s("test"));
        descriptor.iter().for_each(|c| out.extend(c));
        for (k, body) in pictures.iter().enumerate() {
            out.extend(cmd(0, 3, &s(&format!("Sheet {}", k + 1))));
            out.extend(cmd(0, 4, &[]));
            body.iter().for_each(|c| out.extend(c));
            out.extend(cmd(0, 5, &[]));
        }
        out.extend(cmd(0, 2, &[]));
        out
    }

    fn decode1(body: &[Vec<u8>]) -> dxf::Drawing {
        decode(&file(&[], &[body]), 0).unwrap().0
    }

    #[test]
    fn metric_extent_and_y_up() {
        let extent = cmd(2, 6, &ints(&[0, 0, 100, 50]));
        let line = polyline(&[0, 0, 100, 50]);
        let d = decode1(&[extent.clone(), cmd(2, 1, &[&ints(&[1])[..], &0.5f32.to_be_bytes()].concat()), line.clone()]);
        assert_eq!((d.width, d.height, d.units), (50.0, 25.0, "mm"));
        assert_eq!(d.paths[0].points, [[0.0, 25.0], [50.0, 0.0]]);
        // A factor written in the default fixed-point format instead of floating point.
        let d = decode1(&[extent, cmd(2, 1, &ints(&[1, 0, 0x4000])), line.clone()]);
        assert_eq!(d.width, 25.0);
        // An extent running downwards puts VDC (0, 0) at the top.
        let d = decode1(&[cmd(2, 6, &ints(&[0, 50, 100, 0])), polyline(&[0, 0, 0, 50])]);
        assert_eq!((d.paths[0].points[0], d.units), ([0.0, 0.0], ""));
    }

    #[test]
    fn precisions_and_defaults_replacement() {
        let real = |v: &[f64]| v.iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();
        let descriptor = [
            cmd(1, 4, &ints(&[32])),
            cmd(1, 1, &3i32.to_be_bytes()),
            cmd(1, 3, &ints(&[1])),
            // VDC REAL PRECISION: 64-bit floating point, inside METAFILE DEFAULTS REPLACEMENT.
            cmd(1, 12, &cmd(3, 2, &[&ints(&[0])[..], &12i32.to_be_bytes(), &52i32.to_be_bytes()].concat())),
        ];
        let d = decode(&file(&descriptor, &[&[cmd(4, 1, &real(&[0.0, 0.0, 2.5, 1.0]))]]), 0).unwrap().0;
        assert_eq!((d.width, d.height), (2.5, 1.0));
        assert!(d.info.contains(&("CGM version".into(), "3".into())));
    }

    #[test]
    fn colours() {
        let d = decode1(&[
            polyline(&[0, 0, 1, 1]),
            cmd(5, 34, &[2, 255, 0, 0]),
            cmd(5, 4, &[2]),
            polyline(&[0, 0, 1, 1]),
            cmd(2, 2, &ints(&[1])),
            cmd(5, 4, &[0, 0, 255]),
            polyline(&[0, 0, 1, 1]),
            cmd(5, 4, &[255, 255, 255]),
            polyline(&[0, 0, 1, 1]),
        ]);
        let colors: Vec<u32> = d.paths.iter().map(|p| p.color).collect();
        assert_eq!(colors, [FOREGROUND, 0xFF0000, 0x0000FF, BACKGROUND]);
    }

    #[test]
    fn interiors_and_edges() {
        let triangle = cmd(4, 7, &ints(&[0, 0, 10, 0, 0, 10]));
        let solid = cmd(5, 22, &ints(&[1]));
        let d = decode1(&[triangle.clone(), solid.clone(), triangle.clone(), cmd(5, 30, &ints(&[1])), triangle.clone()]);
        // Hollow: outline only; solid: fill; solid with edges: fill and outline.
        assert_eq!((d.fills.len(), d.paths.len()), (2, 2));
        assert_eq!(d.paths[1].points.len(), 4);
        // Polygon set with an invisible edge, no interior: two runs of visible edges.
        let set = cmd(4, 8, &ints(&[0, 0, 1, 10, 0, 1, 10, 10, 0, 0, 10, 3]));
        let d = decode1(&[cmd(5, 22, &ints(&[4])), cmd(5, 30, &ints(&[1])), set]);
        assert_eq!((d.fills.len(), d.paths.len()), (0, 2));
        assert_eq!(d.paths[0].points.len(), 3);
        let d = decode1(&[cmd(5, 22, &ints(&[3])), triangle]);
        assert!(d.info.iter().any(|(_, v)| v.contains("Hatch fill (outline only) × 1")));
    }

    #[test]
    fn arcs() {
        let d = decode1(&[
            cmd(4, 12, &ints(&[0, 0, 10])),
            cmd(4, 13, &ints(&[10, 0, 0, 10, -10, 0])),
            cmd(4, 13, &ints(&[-10, 0, 0, 10, 10, 0])),
            cmd(4, 15, &ints(&[0, 0, 1, 0, 0, 1, 10])),
            cmd(4, 20, &ints(&[0, 0, 1, 0, 0, 1, 10])),
            cmd(4, 18, &ints(&[0, 0, 20, 0, 0, 10, 1, 0, 0, 1])),
        ]);
        let sweeps: Vec<(f64, f64)> = d.arcs.iter().map(|a| (a.t0, a.t1)).collect();
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        let expected = [(0.0, TAU), (0.0, PI), (PI, 0.0), (0.0, FRAC_PI_2), (0.0, -3.0 * FRAC_PI_2), (0.0, FRAC_PI_2)];
        assert!(sweeps.iter().zip(expected).all(|(&a, b)| close(a, b)), "{sweeps:?}");
        assert_eq!((d.arcs[5].u, d.arcs[5].v), ([20.0, 0.0], [0.0, -10.0]));
        // A solid chord: a fill plus the arc and its chord.
        let d = decode1(&[cmd(5, 22, &ints(&[1])), cmd(5, 30, &ints(&[1])), cmd(4, 16, &ints(&[0, 0, 1, 0, 0, 1, 10, 1]))]);
        assert_eq!((d.fills.len(), d.arcs.len(), d.paths.len()), (1, 1, 1));
    }

    #[test]
    fn polybezier() {
        let d = decode1(&[cmd(4, 26, &ints(&[2, 0, 0, 1, 1, 2, 1, 3, 0]))]);
        assert_eq!(d.paths[0].points.len(), BEZIER_STEPS + 1);
        assert_eq!(d.paths[0].points.last(), Some(&[3.0, d.height]));
    }

    #[test]
    fn text() {
        let d = decode1(&[
            cmd(5, 15, &ints(&[5])),
            cmd(5, 18, &ints(&[2, 3, 0, 0, 0, 0])),
            cmd(4, 4, &[&ints(&[10, 10, 0])[..], &s("AB")].concat()),
            cmd(4, 6, &[&ints(&[1])[..], &s("C")].concat()),
        ]);
        let t = &d.texts[0];
        assert_eq!((t.text.as_str(), t.halign, t.valign, t.up, t.x), ("ABC", 1, 2, [0.0, -5.0], [5.0, 0.0]));
        // With Y down and an up vector of (0, -1), text still reads upright.
        let d = decode1(&[cmd(2, 6, &ints(&[0, 100, 100, 0])), cmd(5, 16, &ints(&[0, -1, 1, 0])), cmd(4, 4, &[&ints(&[10, 10, 1])[..], &s("A")].concat())]);
        assert!(d.texts[0].up[1] < 0.0 && d.texts[0].x[0] > 0.0);
    }

    #[test]
    fn pictures_are_pages() {
        let data = file(&[cmd(1, 2, &s("Demo"))], &[&[polyline(&[0, 0, 1, 1])], &[polyline(&[0, 0, 2, 2])]]);
        let (d, pages) = decode(&data, 1).unwrap();
        assert_eq!((pages, d.width), (2, 2.0));
        assert!(d.info.contains(&("Picture".into(), "Sheet 2".into())));
        assert!(d.info.contains(&("Description".into(), "Demo".into())));
        assert!(decode(&data, 2).is_err());
    }

    fn pixels(d: &dxf::Drawing) -> Vec<u32> {
        match &d.images[0].image {
            Image::Color(c) => c.data.clone(),
            Image::Bilevel(_) => panic!("expected colour"),
        }
    }

    #[test]
    fn cell_arrays() {
        let table = cmd(5, 34, &[2, 255, 0, 0]);
        let (black, red) = (rgb(0, 0, 0), rgb(255, 0, 0));
        // Packed: first row from P (0, 10) towards R (20, 10).
        let packed = |p: [i16; 6]| cmd(4, 9, &[&ints(&p)[..], &ints(&[2, 1, 0, 1]), &[1, 2]].concat());
        let d = decode1(&[table.clone(), packed([0, 10, 20, 0, 20, 10])]);
        assert_eq!((d.images[0].pos, d.images[0].px, pixels(&d)), ([0.0, 0.0], 10.0, vec![black, red]));
        // P on the right: the row runs leftwards.
        let d = decode1(&[table.clone(), packed([20, 10, 0, 0, 0, 10])]);
        assert_eq!(pixels(&d), [red, black]);
        // Run length: a run of 2 cells of colour 2.
        let d = decode1(&[table, cmd(4, 9, &[&ints(&[0, 10, 20, 0, 20, 10, 2, 1, 0, 0, 2])[..], &[2]].concat())]);
        assert_eq!(pixels(&d), [red, red]);
    }

    #[test]
    fn group4_tile() {
        use fax::{encoder::Encoder, Color, VecWriter};
        let mut enc = Encoder::new(VecWriter::new());
        for y in 0..4 {
            enc.encode_line((0..8).map(|x| if x == y { Color::Black } else { Color::White }), 8).unwrap();
        }
        let g4 = enc.finish().unwrap().finish();
        let one = [0, 1, 0, 0];
        let begin = [&ints(&[0, 4, 0, 1, 1, 1, 8, 4])[..], &one, &one, &ints(&[0, 0, 8, 4])].concat();
        let tile = [&ints(&[2, 0])[..], &[0, 1, 0], &g4].concat();
        let d = decode1(&[cmd(0, 19, &begin), cmd(4, 28, &tile), cmd(0, 20, &[])]);
        let i = &d.images[0];
        assert_eq!((i.pos, i.px), ([0.0, 0.0], 1.0));
        match &i.image {
            Image::Bilevel(b) => assert!(b.get(2, 2) && !b.get(3, 2)),
            Image::Color(_) => panic!("expected bilevel"),
        }
    }

    #[test]
    fn webcgm_layers() {
        let sdr = s(&String::from_utf8([&ints(&[14, 1])[..], &s("Walls")].concat()).unwrap());
        let d = decode1(&[
            cmd(0, 21, &[&s("l1")[..], &s("layer"), &ints(&[0])].concat()),
            cmd(9, 1, &[&s("layername")[..], &sdr].concat()),
            cmd(0, 22, &[]),
            polyline(&[0, 0, 1, 1]),
            cmd(0, 23, &[]),
            polyline(&[0, 0, 2, 2]),
        ]);
        let names: Vec<&str> = d.paths.iter().map(|p| d.layers[p.layer as usize].name.as_str()).collect();
        assert_eq!(names, ["Walls", "Drawing"]);
    }

    #[test]
    fn rejects_other_files_and_lists_skipped_elements() {
        assert!(decode(b"BEGMF 'x';", 0).err().unwrap().contains("Clear-text"));
        assert!(decode(b"\x00\x00junk", 0).is_err());
        assert!(decode(&file(&[], &[]), 0).is_err());
        let d = decode1(&[cmd(4, 3, &ints(&[0, 0])), polyline(&[0, 0, 1, 1])]);
        assert!(d.info.iter().any(|(k, v)| k == "Not drawn" && v == "Polymarker × 1"));
    }
}
