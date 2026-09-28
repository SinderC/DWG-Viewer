//! W2D ("WHIP!") graphics streams: the 2D content of classic DWF files and of DWF 6 sheets.
//!
//! Opcode layouts follow Autodesk's DWF Toolkit (whiptk). Coordinates are 32-bit logical units,
//! Y up; binary opcodes give them relative to the previous point. When the file says how logical
//! units map to paper (the DWF 6 page descriptor, or `PlotInfo`), output is in paper units.

use std::borrow::Cow;
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::io::Read;

use super::image::{self, Kind};
use super::palette;
use crate::dxf::{Affine, Pen, World, P, BACKGROUND, FOREGROUND};
use crate::hpgl::{pen_color, CAP_PER_EM};

/// Angle units per turn.
const TURN: f64 = 65536.0;
/// Segments of a filled full ellipse.
const ELLIPSE_STEPS: f64 = 128.0;
/// Width scale of 1 in `Font`.
const UNIT_WIDTH: f64 = 1024.0;
const REVISION_OLD_TEXT: u32 = 32;
const REVISION_RGBA: u32 = 33;
const REVISION_NEW_COLORMAP: u32 = 38;
const REVISION_PACKAGE: u32 = 600;

type R<T> = Result<T, String>;
type Point = [i32; 2];

/// Paper units per logical unit, and the paper unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Paper {
    pub scale: f64,
    pub units: &'static str,
}

/// What a stream says about itself, besides its geometry.
pub(super) struct Stream {
    /// "6.00", "0.55", …
    pub version: String,
    pub paper: Option<Paper>,
    pub info: Vec<(String, String)>,
}

/// Draws a W2D stream (starting with `(DWF V` or `(W2D V`) into `world`. `paper` overrides any
/// scale the stream gives itself. A stream that breaks off keeps what was drawn, with a warning row.
pub(super) fn draw(data: &[u8], world: &mut World<'static>, paper: Option<Paper>) -> R<Stream> {
    let version = header(data).ok_or("Not a DWF/W2D graphics stream")?;
    let revision = version.replace('.', "").parse().unwrap_or(0);
    let mut s = State::new(world, revision, paper);
    if let Err(e) = s.run(&data[12..]) {
        s.info.push(("Warning".into(), format!("Graphics stream stopped early: {e}")));
    }
    Ok(Stream { version, paper: s.paper, info: s.info })
}

/// "6.00" from "(DWF V06.00)" or "(W2D V06.00)".
fn header(data: &[u8]) -> Option<String> {
    let h = data.get(..12)?;
    if !(h.starts_with(b"(DWF V") || h.starts_with(b"(W2D V")) || h[8] != b'.' || h[11] != b')' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| h[r.clone()].iter().all(u8::is_ascii_digit).then(|| std::str::from_utf8(&h[r]).unwrap());
    Some(format!("{}.{}", digits(6..8)?.parse::<u32>().ok()?, digits(9..11)?))
}

fn is_space(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | b'\r' | b' ')
}

fn truncated() -> String {
    "unexpected end of data".into()
}

/// Byte cursor over the stream.
struct Cur<'b> {
    d: &'b [u8],
    p: usize,
}

impl<'b> Cur<'b> {
    fn bytes(&mut self, n: usize) -> R<&'b [u8]> {
        let s = self.d.get(self.p..self.p.saturating_add(n)).ok_or_else(truncated)?;
        self.p += n;
        Ok(s)
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> R<i16> {
        Ok(self.u16()? as i16)
    }

    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    /// One byte, or 0 then 256 + a 16-bit value.
    fn count(&mut self) -> R<usize> {
        match self.u8()? {
            0 => Ok(256 + self.u16()? as usize),
            n => Ok(n as usize),
        }
    }

    /// `count` values, each stored as value + 1 (text underscore/overscore positions).
    fn skip_counts(&mut self) -> R<()> {
        for _ in 1..self.count()? {
            self.count()?;
        }
        Ok(())
    }

    fn point32(&mut self) -> R<Point> {
        Ok([self.i32()?, self.i32()?])
    }

    fn point16(&mut self) -> R<Point> {
        Ok([self.i16()? as i32, self.i16()? as i32])
    }

    fn peek(&self) -> Option<u8> {
        self.d.get(self.p).copied()
    }

    fn eat_space(&mut self) {
        while self.peek().is_some_and(is_space) {
            self.p += 1;
        }
    }

    /// An ASCII integer after optional whitespace, as in `C 12` or `R 10,20 5`.
    fn ascii_int(&mut self) -> R<i32> {
        self.eat_space();
        let negative = self.peek() == Some(b'-');
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.p += 1;
            self.eat_space();
        }
        let start = self.p;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.p += 1;
        }
        let digits = std::str::from_utf8(&self.d[start..self.p]).unwrap();
        let v: i64 = digits.parse().map_err(|_| format!("expected a number at byte {start}"))?;
        Ok(if negative { -v } else { v } as i32)
    }

    fn ascii_point(&mut self) -> R<Point> {
        let x = self.ascii_int()?;
        if self.u8()? != b',' {
            return Err(format!("expected ',' at byte {}", self.p - 1));
        }
        Ok([x, self.ascii_int()?])
    }

    /// A string operand: `{` length, UTF-16 units `}`, a quoted string, or a bare word.
    fn string(&mut self) -> R<String> {
        self.eat_space();
        match self.peek().ok_or_else(truncated)? {
            b'{' => {
                self.p += 1;
                let n = self.i32()?.max(0) as usize;
                let units: Vec<u16> = self.bytes(n.checked_mul(2).ok_or_else(truncated)?)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                if self.u8()? != b'}' {
                    return Err("unterminated binary string".into());
                }
                Ok(String::from_utf16_lossy(&units))
            }
            q @ (b'\'' | b'"') => {
                self.p += 1;
                let mut raw = Vec::new();
                loop {
                    match self.u8()? {
                        b'\\' => raw.push(self.u8()?),
                        b if b == q => break,
                        b => raw.push(b),
                    }
                }
                Ok(if q == b'"' { hex_utf16(&raw) } else { latin1(&raw) })
            }
            _ => {
                let start = self.p;
                while self.peek().is_some_and(|b| !is_space(b) && b != b')' && b != b'(') {
                    self.p += 1;
                }
                Ok(latin1(&self.d[start..self.p]))
            }
        }
    }

    /// Opcode name characters after `(`.
    fn name(&mut self) -> &'b str {
        let start = self.p;
        while self.peek().is_some_and(|b| (b'!'..=b'z').contains(&b) && b != b'(' && b != b')') {
            self.p += 1;
        }
        std::str::from_utf8(&self.d[start..self.p]).unwrap()
    }

    /// Skips to the `)` closing the current extended ASCII opcode, the way the toolkit does:
    /// nested parentheses, `'`-quoted strings, `\` escapes and `{` counted binary runs.
    fn skip_group(&mut self) -> R<()> {
        let (mut depth, mut quoted) = (1, false);
        loop {
            match self.u8()? {
                b'\\' => {
                    self.u8()?;
                }
                b'\'' => quoted = !quoted,
                b'(' if !quoted => depth += 1,
                b')' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                b'{' if !quoted => {
                    let n = self.i32()?;
                    if n <= 0 {
                        return Err("binary data of unknown length".into());
                    }
                    self.bytes(n as usize)?;
                }
                _ => {}
            }
        }
    }

    /// Operands of an extended ASCII opcode up to its closing `)`.
    fn group(&mut self) -> R<Vec<Tok>> {
        let mut toks = Vec::new();
        loop {
            self.eat_space();
            match self.peek().ok_or_else(truncated)? {
                b')' => {
                    self.p += 1;
                    return Ok(toks);
                }
                b'(' => {
                    self.p += 1;
                    // Sub-options are named ("(Height 10)"); matrix rows are not ("(1 0 0)").
                    let name = if self.peek().is_some_and(|b| b.is_ascii_alphabetic()) { self.name() } else { "" };
                    toks.push(Tok::Group(name.to_string(), self.group()?));
                }
                b'{' | b'\'' | b'"' => toks.push(Tok::Str(self.string()?)),
                _ => {
                    // "x,y" may have spaces after the comma.
                    let mut word = String::new();
                    loop {
                        let start = self.p;
                        while self.peek().is_some_and(|b| !is_space(b) && b != b'(' && b != b')') {
                            self.p += 1;
                        }
                        word += &latin1(&self.d[start..self.p]);
                        if !word.ends_with(',') {
                            break;
                        }
                        self.eat_space();
                    }
                    toks.push(Tok::Word(word));
                }
            }
        }
    }
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&b| b as char).collect()
}

/// Four hex digits per UTF-16 unit (the `"…"` string form).
fn hex_utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(4).filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok()).collect();
    String::from_utf16_lossy(&units)
}

/// An operand of an extended ASCII opcode.
#[derive(Debug)]
enum Tok {
    Word(String),
    Str(String),
    Group(String, Vec<Tok>),
}

impl Tok {
    fn text(&self) -> Option<&str> {
        match self {
            Tok::Word(s) | Tok::Str(s) => Some(s),
            Tok::Group(..) => None,
        }
    }

    fn num(&self) -> Option<f64> {
        self.text()?.trim_end_matches(',').parse().ok()
    }

    fn int(&self) -> Option<i32> {
        self.num().map(|v| v as i32)
    }

    /// "x,y" or longer comma lists.
    fn ints(&self) -> Option<Vec<i32>> {
        self.text()?.split(',').map(|v| v.trim().parse().ok()).collect()
    }

    fn point(&self) -> Option<Point> {
        match self.ints()?[..] {
            [x, y] => Some([x, y]),
            _ => None,
        }
    }
}

/// Sub-option `name` of an extended ASCII opcode, e.g. `(Height 100)` in `(Font …)`.
fn option<'t>(toks: &'t [Tok], name: &str) -> Option<&'t [Tok]> {
    toks.iter().find_map(|t| match t {
        Tok::Group(n, sub) if n == name => Some(&sub[..]),
        _ => None,
    })
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

fn xy([x, y]: Point) -> P {
    [x as f64, y as f64]
}

enum Flow {
    Next,
    End,
    /// Continue with this data (a decompressed section followed by the rest of the stream).
    Replace(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq)]
enum Coords {
    Ascii,
    Rel16,
    Rel32,
}

struct Font {
    height: i32,
    /// In 1/65536 turns.
    rotation: u16,
    width_scale: u16,
}

struct State<'w> {
    world: &'w mut World<'static>,
    revision: u32,
    paper: Option<Paper>,
    /// `paper` came from outside the stream.
    paper_fixed: bool,
    info: Vec<(String, String)>,
    point: Point,
    map: Vec<u32>,
    color: u32,
    background: u32,
    fill: bool,
    visible: bool,
    weight: i32,
    font: Font,
    /// World layer of the current W2D layer, created on first use.
    layer: Option<u32>,
    layers: HashMap<i32, u32>,
}

impl<'w> State<'w> {
    fn new(world: &'w mut World<'static>, revision: u32, paper: Option<Paper>) -> Self {
        let map = if revision < REVISION_NEW_COLORMAP { palette::OLD_DEFAULT } else { palette::DEFAULT };
        State {
            world,
            revision,
            paper,
            paper_fixed: paper.is_some(),
            info: Vec::new(),
            point: [0; 2],
            map: map.to_vec(),
            color: 0,
            background: 0xFFFFFF,
            fill: false,
            visible: true,
            weight: 0,
            font: Font { height: 0, rotation: 0, width_scale: UNIT_WIDTH as u16 },
            layer: None,
            layers: HashMap::new(),
        }
    }

    fn run(&mut self, data: &[u8]) -> R<()> {
        let mut buf = Cow::Borrowed(data);
        let mut pos = 0;
        loop {
            let mut c = Cur { d: &buf, p: pos };
            c.eat_space();
            if c.peek().is_none() {
                return Ok(());
            }
            let at = c.p;
            match self.opcode(&mut c).map_err(|e| format!("{e} (opcode at byte {at})"))? {
                Flow::Next => pos = c.p,
                Flow::End => return Ok(()),
                Flow::Replace(data) => (buf, pos) = (Cow::Owned(data), 0),
            }
        }
    }

    fn opcode(&mut self, c: &mut Cur) -> R<Flow> {
        let op = c.u8()?;
        match op {
            b'(' => return self.ascii(c),
            b'{' => return self.binary(c),
            0x03 => self.color = self.rgba(c)?,
            b'C' => self.color = self.indexed(c.ascii_int()?),
            b'c' => self.color = self.indexed(c.u8()? as i32),
            0x06 => self.binary_font(c)?,
            b'F' | b'f' => self.fill = op == b'F',
            0x07 | b'g' | 0x11 | b'q' => {
                let coords = if matches!(op, 0x07 | 0x11) { Coords::Rel16 } else { Coords::Rel32 };
                let (points, colors) = self.gouraud(c, coords)?;
                self.shaded(matches!(op, 0x07 | b'g'), points, colors);
            }
            b'G' => {
                c.ascii_int()?;
            }
            0x0B | b'k' => {
                let coords = if op == 0x0B { Coords::Rel16 } else { Coords::Rel32 };
                let counts = (0..c.count()?).map(|_| c.count()).collect::<R<Vec<_>>>()?;
                let points = self.points(c, coords, counts.iter().sum())?;
                self.contours(&counts, points);
            }
            0x0C | b'l' => {
                let (a, b) = if op == 0x0C { (c.point16()?, c.point16()?) } else { (c.point32()?, c.point32()?) };
                let line = vec![self.relative(a), self.relative(b)];
                self.polyline(line);
            }
            b'L' => {
                let line = vec![c.ascii_point()?, c.ascii_point()?];
                self.polyline(line);
            }
            0xAC => {
                let n = c.count()? as i32;
                self.set_layer(n, None);
            }
            0xCC => {
                if c.count()? != 1 {
                    self.skip("Line pattern (drawn solid)");
                }
            }
            0x8D | b'M' | b'm' => {
                let coords = match op {
                    0x8D => Coords::Rel16,
                    b'M' => Coords::Ascii,
                    _ => Coords::Rel32,
                };
                self.point_set(c, coords)?;
                self.skip("Marker");
            }
            b'O' => self.point = c.point32()?,
            0x10 | b'P' | b'p' | 0x14 | b'T' | b't' => {
                let coords = match op {
                    0x10 | 0x14 => Coords::Rel16,
                    b'P' | b'T' => Coords::Ascii,
                    _ => Coords::Rel32,
                };
                let points = self.point_set(c, coords)?;
                match op {
                    0x14 | b'T' | b't' => self.triangles(points),
                    _ if self.fill => self.polygon(points),
                    _ => self.polyline(points),
                }
            }
            b'E' | b'e' | 0x12 | b'R' | b'r' | 0x92 => self.binary_ellipse(c, op)?,
            b'S' => {
                c.ascii_int()?;
            }
            b's' => {
                c.i32()?;
            }
            b'V' | b'v' => self.visible = op == b'V',
            0x17 => self.weight = c.i32()?,
            0x18 | b'x' => self.binary_text(c, op)?,
            b'N' => {
                c.i32()?;
            }
            b'n' => {
                c.i16()?;
            }
            0x0E => {}
            _ => return Err(format!("unsupported opcode 0x{op:02X}")),
        }
        Ok(Flow::Next)
    }

    /// `{` size opcode payload `}`: the size counts the 2-byte opcode and the payload with its `}`.
    fn binary(&mut self, c: &mut Cur) -> R<Flow> {
        let size = c.i32()?;
        let op = c.u16()?;
        match op {
            0x0011 => return self.inflate(c).map(Flow::Replace),
            0x0010 | 0x0123 => return Err("LZ-compressed data (DWF before 0.39) is not supported".into()),
            _ if size < 2 => return Err("binary opcode of unknown length".into()),
            _ => {}
        }
        let payload = c.bytes(size as usize - 2)?;
        match op {
            0x0001 => {
                let n = match payload.first() {
                    Some(0) => 256,
                    Some(&n) => n as usize,
                    None => return Err(truncated()),
                };
                let colors = payload.get(1..1 + 4 * n).ok_or_else(truncated)?;
                self.map = colors.chunks_exact(4).map(|b| self.rgba_bytes(b)).collect();
            }
            0x0002..=0x0009 | 0x000C | 0x000D => self.image(op, payload)?,
            _ => {}
        }
        Ok(Flow::Next)
    }

    /// An image opcode's payload: size, corners (relative), identifier, [colour map], data.
    fn image(&mut self, op: u16, payload: &[u8]) -> R<()> {
        let mut c = Cur { d: payload, p: 0 };
        let (width, height) = (c.u16()? as u32, c.u16()? as u32);
        let (min, max) = (c.point32()?, c.point32()?);
        let (min, max) = (self.relative(min), self.relative(max));
        c.i32()?; // identifier
        let own_map = matches!(op, 0x0002 | 0x0003 | 0x0005 | 0x000D);
        let map = if own_map {
            let n = match c.u8()? {
                0 => 256,
                n => n as usize,
            };
            (0..n).map(|_| self.rgba(&mut c)).collect::<R<Vec<_>>>()?
        } else {
            self.map.clone()
        };
        let size = c.i32()?.max(0) as usize;
        let data = c.bytes(size)?;
        let kind = match op {
            0x0004 => Kind::Indexed,
            0x0005 => Kind::Mapped,
            0x0006 => Kind::Rgb,
            0x0007 => Kind::Rgba,
            0x0008 => Kind::Jpeg,
            0x0009 => Kind::Group4,
            0x000C => Kind::Png,
            0x000D => Kind::Group4Mapped,
            _ => {
                self.skip("Raster image (bitonal or Group 3X)");
                return Ok(());
            }
        };
        if !self.visible {
            return Ok(());
        }
        let decoded = image::decode(kind, width, height, &map, data);
        let Ok(img) = decoded else {
            self.world.skip("Raster image (unreadable)");
            return Ok(());
        };
        // JPEG and PNG carry their own size; fit their width to the placement.
        let (w, _) = img.size();
        let s = self.scale();
        let layer = self.pen().layer;
        self.world.image(img, [min[0] as f64 * s, max[1] as f64 * s], (max[0] - min[0]) as f64 / w as f64 * s, layer);
        Ok(())
    }

    /// Inflates a zlib section; the stream continues after its closing `}`.
    fn inflate(&mut self, c: &mut Cur) -> R<Vec<u8>> {
        let rest = &c.d[c.p..];
        let mut z = flate2::bufread::ZlibDecoder::new(rest);
        let mut out = Vec::new();
        z.read_to_end(&mut out).map_err(|e| format!("corrupt compressed data: {e}"))?;
        let mut after = &rest[z.total_in() as usize..];
        if after.first() == Some(&b'}') {
            after = &after[1..];
        }
        out.extend_from_slice(after);
        Ok(out)
    }

    fn ascii(&mut self, c: &mut Cur) -> R<Flow> {
        let name = c.name();
        let known = matches!(
            name,
            "Circle" | "Ellipse" | "Contour" | "Gouraud" | "GourLine" | "Text" | "Font" | "Layer" | "Color" | "ColorMap" | "Background"
                | "LineWeight" | "PlotInfo" | "Title" | "Author" | "Creator" | "SourceFilename" | "EndOfDWF"
        );
        if !known {
            if name == "Image" || name == "Group4PNGImage" {
                self.skip("Raster image");
            }
            c.skip_group()?;
            return Ok(Flow::Next);
        }
        let t = c.group()?;
        let bad = || format!("malformed ({name} …)");
        match name {
            "Circle" | "Ellipse" => {
                let centre = t.first().and_then(Tok::point).ok_or_else(bad)?;
                let (major, minor) = match name {
                    "Circle" => (t.get(1).and_then(Tok::int).ok_or_else(bad)?, None),
                    _ => t.get(1).and_then(Tok::point).map(|[a, b]| (a, Some(b))).ok_or_else(bad)?,
                };
                let [start, end] = t.get(2).and_then(Tok::point).unwrap_or([0, 0x10000]);
                let tilt = t.get(3).and_then(Tok::int).unwrap_or(0);
                self.ellipse(centre, major, minor.unwrap_or(major), start as u32 & 0xFFFF, end as u32 & 0x1FFFF, tilt as u16, true);
            }
            "Contour" => {
                let n = t.first().and_then(Tok::int).ok_or_else(bad)?.max(0) as usize;
                let counts: Vec<usize> = t.iter().skip(1).take(n).map(|t| t.int().map(|v| v.max(0) as usize)).collect::<Option<_>>().ok_or_else(bad)?;
                let points: Vec<Point> = t.iter().skip(1 + n).filter_map(Tok::point).collect();
                self.contours(&counts, points);
            }
            "Gouraud" | "GourLine" => {
                let (mut points, mut colors) = (Vec::new(), Vec::new());
                for pair in t.get(1..).unwrap_or_default().chunks_exact(2) {
                    points.push(pair[0].point().ok_or_else(bad)?);
                    colors.push(self.ascii_color(&pair[1]).ok_or_else(bad)?);
                }
                self.shaded(name == "Gouraud", points, colors);
            }
            "Text" => {
                let pos = t.first().and_then(Tok::point).ok_or_else(bad)?;
                let text = t.get(1).and_then(Tok::text).ok_or_else(bad)?.to_string();
                self.text(pos, text);
            }
            "Font" => {
                let value = |name| option(&t, name).and_then(|o| o.first()).and_then(Tok::int);
                if let Some(h) = value("Height") {
                    self.font.height = h;
                }
                if let Some(r) = value("Rotation") {
                    self.font.rotation = r as u16;
                }
                if let Some(w) = value("Widthscale") {
                    self.font.width_scale = w as u16;
                }
            }
            "Layer" => {
                let n = t.first().and_then(Tok::int).ok_or_else(bad)?;
                self.set_layer(n, t.get(1).and_then(Tok::text).filter(|s| !s.is_empty()));
            }
            "Color" => self.color = t.first().and_then(|t| self.ascii_color(t)).ok_or_else(bad)?,
            "Background" => self.background = t.first().and_then(|t| self.ascii_color(t)).ok_or_else(bad)?,
            "ColorMap" => self.map = t.iter().skip(1).map(|t| self.ascii_color(t)).collect::<Option<_>>().ok_or_else(bad)?,
            "LineWeight" => self.weight = t.first().and_then(Tok::int).ok_or_else(bad)?,
            "PlotInfo" => self.plot_info(&t),
            "EndOfDWF" => return Ok(Flow::End),
            _ => {
                let label = if name == "SourceFilename" { "Source file" } else { name };
                if let Some(v) = t.first().and_then(Tok::text).filter(|v| !v.trim().is_empty()) {
                    self.info.push((label.to_string(), v.trim().to_string()));
                }
            }
        }
        Ok(Flow::Next)
    }

    /// `(PlotInfo show|hide [rotation] mm|in width height llx lly urx ury ((a b c)(d e f)(g h i)))`:
    /// the paper size and the logical-to-paper transform.
    fn plot_info(&mut self, t: &[Tok]) {
        let Some(u) = t.iter().position(|t| matches!(t.text(), Some("mm" | "in"))) else { return };
        let units = if t[u].text() == Some("mm") { "mm" } else { "in" };
        if let (Some(w), Some(h)) = (t.get(u + 1).and_then(Tok::num), t.get(u + 2).and_then(Tok::num)) {
            self.info.push(("Paper".into(), format!("{} × {} {units}", round(w), round(h))));
        }
        let row = t.iter().find_map(|t| match t {
            Tok::Group(n, rows) if n.is_empty() => match rows.first() {
                Some(Tok::Group(_, row)) => Some(row),
                _ => None,
            },
            _ => None,
        });
        let scale = row.and_then(|r| Some(r.first()?.num()?.hypot(r.get(1)?.num()?)));
        if let Some(scale) = scale.filter(|s| s.is_finite() && *s > 0.0) {
            if !self.paper_fixed {
                self.paper = Some(Paper { scale, units });
            }
        }
    }

    fn rgba_bytes(&self, b: &[u8]) -> u32 {
        if self.revision < REVISION_RGBA { rgb(b[2], b[1], b[0]) } else { rgb(b[0], b[1], b[2]) }
    }

    fn rgba(&self, c: &mut Cur) -> R<u32> {
        Ok(self.rgba_bytes(c.bytes(4)?))
    }

    fn indexed(&self, i: i32) -> u32 {
        usize::try_from(i).ok().and_then(|i| self.map.get(i)).copied().unwrap_or(0)
    }

    /// "r,g,b,a" or a colour map index.
    fn ascii_color(&self, t: &Tok) -> Option<u32> {
        match t.ints()?[..] {
            [i] => Some(self.indexed(i)),
            [r, g, b, _] => Some(rgb(r as u8, g as u8, b as u8)),
            _ => None,
        }
    }

    fn binary_font(&mut self, c: &mut Cur) -> R<()> {
        if self.revision < 31 {
            // Name, style, charset, pitch and family: no size.
            let n = c.count()?;
            c.bytes(n + 1 + 4 + 4)?;
            return Ok(());
        }
        let fields = c.u16()?;
        if fields & 0x0001 != 0 {
            c.string()?;
        }
        for bit in [0x0002, 0x0004, 0x0008, 0x0010] {
            if fields & bit != 0 {
                c.u8()?;
            }
        }
        if fields & 0x0020 != 0 {
            self.font.height = c.i32()?;
        }
        if fields & 0x0040 != 0 {
            self.font.rotation = c.u16()?;
        }
        if fields & 0x0080 != 0 {
            self.font.width_scale = c.u16()?;
        }
        for bit in [0x0100, 0x0200] {
            if fields & bit != 0 {
                c.u16()?;
            }
        }
        if fields & 0x0400 != 0 {
            c.i32()?;
        }
        Ok(())
    }

    fn binary_text(&mut self, c: &mut Cur, op: u8) -> R<()> {
        if self.revision < REVISION_OLD_TEXT {
            return Err("text in DWF before 0.32 is not supported".into());
        }
        let pos = c.point32()?;
        let pos = self.relative(pos);
        let text = c.string()?;
        if op == 0x18 {
            c.skip_counts()?; // overscore
            c.skip_counts()?; // underscore
            c.bytes(4 * 8)?; // bounds
            if self.revision >= REVISION_PACKAGE {
                c.skip_counts()?;
            }
        }
        self.text(pos, text);
        Ok(())
    }

    fn binary_ellipse(&mut self, c: &mut Cur, op: u8) -> R<()> {
        let centre = match op {
            b'E' | b'R' => c.ascii_point()?,
            0x12 => {
                let p = c.point16()?;
                self.relative(p)
            }
            _ => {
                let p = c.point32()?;
                self.relative(p)
            }
        };
        let (major, minor, start, end, tilt, explicit) = match op {
            b'E' => {
                let [a, b] = c.ascii_point()?;
                (a, b, 0, 0x10000, 0, false)
            }
            b'R' => {
                let r = c.ascii_int()?;
                (r, r, 0, 0x10000, 0, false)
            }
            b'r' => {
                let r = c.i32()?;
                (r, r, 0, 0x10000, 0, false)
            }
            0x12 => {
                let r = c.u16()? as i32;
                (r, r, 0, 0x10000, 0, false)
            }
            0x92 => {
                let r = c.i32()?;
                (r, r, c.u16()? as u32, c.u16()? as u32, 0, true)
            }
            _ => (c.i32()?, c.i32()?, c.u16()? as u32, c.u16()? as u32, c.u16()?, true),
        };
        self.ellipse(centre, major, minor, start, end, tilt, explicit);
        Ok(())
    }

    fn relative(&mut self, [dx, dy]: Point) -> Point {
        self.point = [self.point[0].wrapping_add(dx), self.point[1].wrapping_add(dy)];
        self.point
    }

    /// `n` points: absolute in ASCII, else relative to the previous point.
    fn points(&mut self, c: &mut Cur, coords: Coords, n: usize) -> R<Vec<Point>> {
        let mut out = Vec::with_capacity(n.min(c.d.len()));
        for _ in 0..n {
            out.push(match coords {
                Coords::Ascii => c.ascii_point()?,
                Coords::Rel16 => {
                    let p = c.point16()?;
                    self.relative(p)
                }
                Coords::Rel32 => {
                    let p = c.point32()?;
                    self.relative(p)
                }
            });
        }
        Ok(out)
    }

    fn point_set(&mut self, c: &mut Cur, coords: Coords) -> R<Vec<Point>> {
        let n = if coords == Coords::Ascii { c.ascii_int()?.max(0) as usize } else { c.count()? };
        self.points(c, coords, n)
    }

    /// Gouraud points: each followed by an RGBA colour.
    fn gouraud(&mut self, c: &mut Cur, coords: Coords) -> R<(Vec<Point>, Vec<u32>)> {
        let n = c.count()?;
        let (mut points, mut colors) = (Vec::new(), Vec::new());
        for _ in 0..n {
            points.extend(self.points(c, coords, 1)?);
            colors.push(self.rgba(c)?);
        }
        Ok((points, colors))
    }

    fn set_layer(&mut self, n: i32, name: Option<&str>) {
        let id = match (self.layers.get(&n), name) {
            (Some(&id), None) => id,
            (_, name) => {
                let id = self.world.layer(name.unwrap_or(&format!("Layer {n}")), FOREGROUND);
                *self.layers.entry(n).or_insert(id)
            }
        };
        self.layer = Some(id);
    }

    fn skip(&mut self, kind: &'static str) {
        if self.visible {
            self.world.skip(kind);
        }
    }

    fn scale(&self) -> f64 {
        self.paper.map_or(1.0, |p| p.scale)
    }

    fn pen_for(&mut self, color: u32) -> Pen {
        let layer = match self.layer {
            Some(l) => l,
            None => *self.layer.insert(self.world.layer("0", FOREGROUND)),
        };
        // Black and white follow the viewer's paper; on a dark background the file's white is the ink.
        let dark = (self.background >> 16 & 0xFF) + (self.background >> 8 & 0xFF) + (self.background & 0xFF) < 3 * 128;
        let color = match color {
            0xFFFFFF if dark => FOREGROUND,
            0 if dark => BACKGROUND,
            c => pen_color(c),
        };
        Pen { color, layer, width: self.weight.max(0) as f64 * self.scale() }
    }

    fn pen(&mut self) -> Pen {
        self.pen_for(self.color)
    }

    fn m(&self) -> Affine {
        Affine::scale(self.scale(), self.scale())
    }

    fn polyline(&mut self, points: Vec<Point>) {
        if self.visible {
            let pen = self.pen();
            self.world.path(points.into_iter().map(xy).collect(), &self.m(), pen);
        }
    }

    fn polygon(&mut self, points: Vec<Point>) {
        if self.visible {
            let pen = self.pen();
            self.world.fill(vec![points.into_iter().map(xy).collect()], false, &self.m(), pen);
        }
    }

    /// A triangle strip.
    fn triangles(&mut self, points: Vec<Point>) {
        if self.visible {
            let pen = self.pen();
            let rings = points.windows(3).map(|t| t.iter().copied().map(xy).collect()).collect();
            self.world.fill(rings, false, &self.m(), pen);
        }
    }

    /// Contours are filled together, so inner ones are holes.
    fn contours(&mut self, counts: &[usize], points: Vec<Point>) {
        if !self.visible {
            return;
        }
        let mut rest = &points[..];
        let mut rings = Vec::new();
        for &n in counts {
            let (ring, tail) = rest.split_at(n.min(rest.len()));
            rings.push(ring.iter().copied().map(xy).collect());
            rest = tail;
        }
        let pen = self.pen();
        self.world.fill(rings, true, &self.m(), pen);
    }

    /// Gouraud-shaded triangle strip or polyline, in the colour of its first vertex.
    fn shaded(&mut self, strip: bool, points: Vec<Point>, colors: Vec<u32>) {
        if !self.visible || points.is_empty() {
            return;
        }
        self.world.skip("Gouraud shading (drawn flat)");
        let pen = self.pen_for(colors[0]);
        let m = self.m();
        if strip {
            let rings = points.windows(3).map(|t| t.iter().copied().map(xy).collect()).collect();
            self.world.fill(rings, false, &m, pen);
        } else {
            self.world.path(points.into_iter().map(xy).collect(), &m, pen);
        }
    }

    /// Angles in 1/65536 turns, counter-clockwise from the major axis; `end` ≤ `start` wraps.
    /// `explicit` angles in files before 0.31 used an inclusive end.
    #[allow(clippy::too_many_arguments)]
    fn ellipse(&mut self, centre: Point, major: i32, minor: i32, start: u32, mut end: u32, tilt: u16, explicit: bool) {
        if explicit && self.revision <= 30 {
            if start == end {
                return;
            }
            if end < 0x10000 {
                end += 1;
            }
        }
        if end <= start {
            end += 0x10000;
        }
        if !self.visible || major <= 0 {
            return;
        }
        let angle = |a: u32| a as f64 / TURN * TAU;
        let (t0, t1) = (angle(start), angle(end));
        let (s, c) = angle(tilt as u32).sin_cos();
        let (major, ratio) = (major as f64, minor.max(0) as f64 / major as f64);
        let u = [major * c, major * s];
        let v = [-u[1] * ratio, u[0] * ratio];
        let centre = xy(centre);
        let (pen, m) = (self.pen(), self.m());
        if self.fill {
            let steps = ((t1 - t0) / TAU * ELLIPSE_STEPS).ceil().max(2.0) as usize;
            let mut ring: Vec<P> = (0..=steps)
                .map(|k| {
                    let (s, c) = (t0 + (t1 - t0) * k as f64 / steps as f64).sin_cos();
                    [centre[0] + u[0] * c + v[0] * s, centre[1] + u[1] * c + v[1] * s]
                })
                .collect();
            if end - start < 0x10000 {
                ring.push(centre);
            }
            self.world.fill(vec![ring], false, &m, pen);
        } else {
            self.world.ellipse(centre, u, ratio, t0, t1, &m, pen);
        }
    }

    /// Text on its baseline from `pos`, sized and turned by the current font.
    fn text(&mut self, pos: Point, text: String) {
        if !self.visible || text.trim().is_empty() {
            return;
        }
        if self.font.height <= 0 {
            self.world.skip("Text without a font size");
            return;
        }
        let pen = self.pen();
        let rotation = self.font.rotation as f64 / TURN * TAU;
        let width = self.font.width_scale as f64 / UNIT_WIDTH;
        let height = self.font.height as f64 * CAP_PER_EM;
        self.world.push_text(xy(pos), rotation, height, width, (0, 0), text, &self.m(), pen);
    }
}

/// Up to four significant decimals, without trailing zeros.
pub(super) fn round(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::dxf::Drawing;

    /// A W2D 6.00 stream from `body`.
    pub(crate) fn stream(body: &[u8]) -> Vec<u8> {
        [b"(W2D V06.00)".as_slice(), body].concat()
    }

    pub(crate) fn render(data: &[u8]) -> (Drawing, Stream) {
        let mut world = World::new(Vec::new());
        let s = draw(data, &mut world, None).unwrap();
        let info = s.info.clone();
        (world.finish(s.paper.map_or("", |p| p.units), info).unwrap(), s)
    }

    fn le(v: &[i32]) -> Vec<u8> {
        v.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn le16(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn header_versions() {
        assert_eq!(header(b"(DWF V00.55)").as_deref(), Some("0.55"));
        assert_eq!(header(b"(W2D V06.00)").as_deref(), Some("6.00"));
        assert_eq!(header(b"(DWF V6.000)"), None);
        assert_eq!(header(b"PK\x03\x04"), None);
    }

    #[test]
    fn ascii_and_relative_lines() {
        // ASCII line (absolute), then a 32-bit relative line from the origin set by 'O'.
        let mut body = b"L 0,0 100,0 ".to_vec();
        body.push(b'O');
        body.extend(le(&[100, 0]));
        body.push(b'l');
        body.extend(le(&[0, 0, 0, 50]));
        body.push(0x0C);
        body.extend(le16(&[10, 0, 0, 10]));
        let (d, s) = render(&stream(&body));
        assert_eq!(s.version, "6.00");
        assert_eq!(d.paths.len(), 3);
        // Y is flipped against the top of the extents (60).
        assert_eq!(d.paths[1].points, vec![[100.0, 60.0], [100.0, 10.0]]);
        assert_eq!(d.paths[2].points, vec![[110.0, 10.0], [110.0, 0.0]]);
    }

    #[test]
    fn polyline_polygon_and_fill_mode() {
        let mut body = vec![b'p', 3];
        body.extend(le(&[0, 0, 10, 0, 0, 10]));
        body.push(b'F');
        body.push(0x10);
        body.push(3);
        body.extend(le16(&[5, 5, 1, 0, 0, 1]));
        body.push(b'f');
        let (d, _) = render(&stream(&body));
        assert_eq!(d.paths.len(), 1);
        assert_eq!(d.paths[0].points.len(), 3);
        assert_eq!(d.fills.len(), 1);
        assert_eq!(d.fills[0].rings[0].len(), 3);
    }

    #[test]
    fn colors_layers_and_weight() {
        let mut body = b"(Layer 1 'WALLS')(Color 255,0,0,255)".to_vec();
        body.extend([0x17]);
        body.extend(le(&[4]));
        body.extend(b"L 0,0 10,10 ");
        body.extend([0xAC, 2]);
        body.extend([b'c', 2]);
        body.extend(b"L 0,0 5,0 ");
        let mut rgba = vec![0x03, 0, 0, 0, 255];
        rgba.extend(b"L 0,0 1,1 (Layer 1)L 0,0 2,2 ");
        body.extend(rgba);
        let (d, _) = render(&stream(&body));
        let names: Vec<_> = d.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["WALLS", "Layer 2"]);
        assert_eq!(d.paths[0].color, 0xFF0000);
        assert_eq!(d.paths[0].width, 4.0);
        assert_eq!(d.paths[1].color, palette::DEFAULT[2]);
        assert_eq!(d.paths[2].color, FOREGROUND, "black ink follows the viewer");
        assert_eq!(d.layers[d.paths[3].layer as usize].name, "WALLS");
    }

    #[test]
    fn circles_arcs_and_ellipses() {
        let mut body = vec![b'r'];
        body.extend(le(&[100, 100, 50]));
        body.push(0x92);
        body.extend(le(&[0, 0, 20]));
        body.extend(le16(&[0, 0x4000]));
        body.extend(b"(Ellipse 0,0 40,10 0,65536 16384)");
        let (d, _) = render(&stream(&body));
        assert_eq!(d.arcs.len(), 3);
        let quarter = &d.arcs[1];
        assert!((quarter.t1 - quarter.t0 - TAU / 4.0).abs() < 1e-9);
        // A 90° tilt turns the major axis vertical (Y is flipped in the output).
        let e = &d.arcs[2];
        assert!(e.u[0].abs() < 1e-9 && (e.u[1] + 40.0).abs() < 1e-9);
    }

    #[test]
    fn filled_circle_is_a_fill() {
        let (d, _) = render(&stream(b"F R 0,0 10 f"));
        assert!(d.arcs.is_empty());
        assert!(d.fills[0].rings[0].len() > 64);
    }

    #[test]
    fn text_uses_the_font() {
        let mut body = b"(Font (Name 'Arial')(Height 100)(Rotation 16384))(Text 10,20 'Hello')".to_vec();
        body.push(b'x');
        body.extend(le(&[0, 0]));
        body.extend(b"'World'");
        let (d, _) = render(&stream(&body));
        assert_eq!(d.texts.len(), 2);
        assert_eq!(d.texts[0].text, "Hello");
        assert_eq!(d.texts[1].text, "World");
        // Rotated 90°: the baseline points up (negative Y in output).
        assert!(d.texts[0].x[0].abs() < 1e-9 && d.texts[0].x[1] < 0.0);
    }

    #[test]
    fn binary_font_and_complex_text() {
        let mut body = vec![0x06];
        body.extend(0x0061u16.to_le_bytes()); // name, height, rotation
        body.extend(b"'Arial'");
        body.extend(le(&[50]));
        body.extend(0u16.to_le_bytes());
        body.push(0x18);
        body.extend(le(&[5, 5]));
        body.extend(b"{");
        body.extend(le(&[2]));
        body.extend(le16(&[0x48, 0x69]));
        body.extend(b"}");
        body.extend([1, 1]); // no overscore, no underscore
        body.extend(le(&[0; 8]));
        body.push(1); // no reserved values
        let (d, _) = render(&stream(&body));
        assert_eq!(d.texts[0].text, "Hi");
        let cap = d.texts[0].up[0].hypot(d.texts[0].up[1]);
        assert!((cap - 50.0 * CAP_PER_EM).abs() < 1e-9);
    }

    #[test]
    fn contour_set_has_holes() {
        let body = b"(Contour 2 4 4 0,0 10,0 10,10 0,10 2,2 8,2 8,8 2,8)";
        let (d, _) = render(&stream(body));
        assert_eq!(d.fills[0].rings.len(), 2);
        assert!(d.fills[0].even_odd);
    }

    #[test]
    fn color_map_replaces_palette() {
        let mut body = b"{".to_vec();
        body.extend(le(&[2 + 1 + 8 + 1]));
        body.extend(1u16.to_le_bytes());
        body.extend([2, 0, 0, 0, 255, 0, 0, 255, 255, b'}']);
        body.extend([b'c', 1]);
        body.extend(b"L 0,0 1,1 ");
        let (d, _) = render(&stream(&body));
        assert_eq!(d.paths[0].color, 0x0000FF);
    }

    #[test]
    fn zlib_section() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(b"L 0,0 10,0 ").unwrap();
        let mut body = b"{".to_vec();
        body.extend(le(&[0]));
        body.extend(0x0011u16.to_le_bytes());
        body.extend(z.finish().unwrap());
        body.extend(b"}L 0,0 0,10 (EndOfDWF)L 5,5 6,6 ");
        let (d, _) = render(&stream(&body));
        assert_eq!(d.paths.len(), 2);
    }

    #[test]
    fn plot_info_scales_to_paper() {
        let body = b"(PlotInfo show 0 mm 297 210 0 0 0 0 ((0.01 0 0)(0 0.01 0)(0 0 1)))L 0,0 1000,0 L 0,0 0,500 ";
        let (d, s) = render(&stream(body));
        assert_eq!(s.paper, Some(Paper { scale: 0.01, units: "mm" }));
        assert_eq!(d.units, "mm");
        assert!((d.width - 10.0).abs() < 1e-9 && (d.height - 5.0).abs() < 1e-9);
        assert!(s.info.contains(&("Paper".into(), "297 × 210 mm".into())));
    }

    #[test]
    fn unknown_opcodes_are_skipped_and_breaks_warn() {
        let mut body = b"(Author 'Me')(Guid 'x' (nested) 'it''s')".to_vec();
        body.extend(b"{");
        body.extend(le(&[5]));
        body.extend(0x0027u16.to_le_bytes());
        body.extend(b"ab}");
        body.extend(b"L 0,0 1,1 ");
        body.push(0xFF);
        let (d, s) = render(&stream(&body));
        assert_eq!(d.paths.len(), 1);
        assert!(s.info.contains(&("Author".into(), "Me".into())));
        assert!(s.info.iter().any(|(k, v)| k == "Warning" && v.contains("0xFF")));
    }

    #[test]
    fn rgb_image_is_placed_by_its_corners() {
        let mut payload = Vec::new();
        payload.extend(2u16.to_le_bytes());
        payload.extend(1u16.to_le_bytes());
        payload.extend(le(&[10, 20, 20, 5])); // min (10, 20), max (30, 25): relative
        payload.extend(le(&[7, 6])); // identifier, data size
        payload.extend([255, 0, 0, 0, 0, 255, b'}']);
        let mut body = b"{".to_vec();
        body.extend(le(&[2 + payload.len() as i32]));
        body.extend(6u16.to_le_bytes());
        body.extend(payload);
        body.extend(b"L 0,0 1,1 ");
        let (d, _) = render(&stream(&body));
        assert_eq!(d.images.len(), 1);
        let i = &d.images[0];
        assert_eq!(i.px, 10.0);
        // Top-left (10, 25) against extents from (0, 0) to (30, 25).
        assert_eq!(i.pos, [10.0, 0.0]);
        assert_eq!(i.image.size(), (2, 1));
    }

    #[test]
    fn dark_background_swaps_black_and_white() {
        let (d, _) = render(&stream(b"(Background 0,0,0,255)(Color 255,255,255,255)L 0,0 1,1 "));
        assert_eq!(d.paths[0].color, FOREGROUND);
    }

    #[test]
    fn invisible_geometry_is_not_drawn() {
        let (d, _) = render(&stream(b"v L 0,0 1,1 V L 0,0 2,2 "));
        assert_eq!(d.paths.len(), 1);
    }

    #[test]
    fn strings_hex_and_escapes() {
        let mut c = Cur { d: b"\"00480069\" 'a\\'b' word)", p: 0 };
        assert_eq!(c.string().unwrap(), "Hi");
        assert_eq!(c.string().unwrap(), "a'b");
        assert_eq!(c.string().unwrap(), "word");
    }
}
