//! PJL / PCL / HP RTL wrapper around HP-GL/2: finds the HP-GL/2 blocks and decodes RTL raster
//! images. PCL text and page formatting are ignored.

use crate::bitmap::{Bitmap, MAX_BILEVEL_PIXELS};
use crate::rgba::{rgb, RgbaImage, MAX_PIXELS};
use crate::tiff::Image;

const ESC: u8 = 0x1B;
/// Upper bound on rows, so `ESC*b#Y` offsets cannot allocate without limit.
const MAX_ROWS: usize = 1 << 20;
const WHITE: u32 = 0xFFFFFF;
/// Default palette: white, black, red, green, yellow, blue, magenta, cyan.
const PALETTE: [u32; 8] = [WHITE, 0x000000, 0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF];

pub enum Event<'a> {
    Hpgl(&'a [u8]),
    Image(RtlImage),
    /// A raster image that is not drawn, and why.
    Skip(&'static str),
}

/// A raster image; `x`, `y` are its top-left corner in inches from the top-left of the page.
pub struct RtlImage {
    pub image: Image,
    pub x: f64,
    pub y: f64,
    pub dpi: f64,
}

/// True if the data starts with a PCL/PJL escape (not an HP-GL/1 `ESC .` device-control sequence).
pub fn is_wrapped(data: &[u8]) -> bool {
    let start = data.iter().position(|b| !b.is_ascii_whitespace() && *b != 0).unwrap_or(data.len());
    data.get(start) == Some(&ESC) && data.get(start + 1) != Some(&b'.')
}

/// Calls `f` with the HP-GL blocks and raster images in file order. Returns whether a PCL or PJL
/// wrapper was found.
pub fn split<'a>(data: &'a [u8], mut f: impl FnMut(Event<'a>)) -> bool {
    let mut s = State { mode: if is_wrapped(data) { Mode::Pcl } else { Mode::Hpgl }, wrapped: false, rtl: Rtl::default() };
    let mut i = 0;
    while i < data.len() {
        if data[i] == ESC && data.get(i + 1) != Some(&b'.') {
            i = escape(data, i, |e| s.escape(e, &mut f));
            continue;
        }
        let end = data[i..].windows(2).position(|w| w[0] == ESC && w[1] != b'.').map_or(data.len(), |n| i + n);
        match s.mode {
            Mode::Hpgl => {
                f(Event::Hpgl(&data[i..end]));
                i = end;
            }
            Mode::Pjl => i = s.pjl(&data[..end], i),
            Mode::Pcl => i = end,
        }
    }
    s.rtl.finish(&mut f);
    s.wrapped
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Hpgl,
    Pcl,
    Pjl,
}

/// One PCL command: `ESC param group value term`, with the data bytes that follow `…W` commands.
/// Two-character escapes (`ESC E`) have `group` and `term` 0.
struct Esc<'a> {
    param: u8,
    group: u8,
    value: f64,
    /// The value had an explicit sign (relative cursor moves).
    signed: bool,
    /// Upper-case terminator.
    term: u8,
    data: &'a [u8],
}

/// Parses the escape sequence at `i`, calling `f` for each command of a combined sequence
/// (`ESC*r1s2A`). Returns the position after it.
fn escape<'a>(data: &'a [u8], i: usize, mut f: impl FnMut(Esc<'a>)) -> usize {
    let mut j = i + 1;
    let Some(&param) = data.get(j) else { return j };
    j += 1;
    if !(0x21..=0x2F).contains(&param) {
        f(Esc { param, group: 0, value: 0.0, signed: false, term: 0, data: &[] });
        return j;
    }
    let group = match data.get(j) {
        Some(&g) if (0x60..=0x7E).contains(&g) => {
            j += 1;
            g
        }
        _ => 0,
    };
    loop {
        let start = j;
        while j < data.len() && matches!(data[j], b'+' | b'-' | b'.' | b'0'..=b'9') {
            j += 1;
        }
        let text = &data[start..j];
        let value: f64 = std::str::from_utf8(text).ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let signed = matches!(text.first(), Some(b'+' | b'-'));
        let Some(&t) = data.get(j).filter(|t| (0x40..=0x7E).contains(*t)) else { return j };
        j += 1;
        let term = t.to_ascii_uppercase();
        let payload = if term == b'W' || (param == b'&' && group == b'p' && term == b'X') {
            let end = j.saturating_add(value.max(0.0) as usize).min(data.len());
            let p = &data[j..end];
            j = end;
            p
        } else {
            &[]
        };
        f(Esc { param, group, value, signed, term, data: payload });
        if t == term {
            return j;
        }
    }
}

struct State {
    mode: Mode,
    wrapped: bool,
    rtl: Rtl,
}

impl State {
    fn escape<'a>(&mut self, e: Esc, f: &mut impl FnMut(Event<'a>)) {
        match (e.param, e.term) {
            // Universal Exit Language: PJL follows.
            (b'%', b'X') => {
                self.rtl.finish(f);
                self.mode = Mode::Pjl;
                self.wrapped = true;
            }
            (b'%', b'B') => {
                self.mode = Mode::Hpgl;
                self.wrapped = true;
            }
            (b'%', b'A') => self.mode = Mode::Pcl,
            (b'E', 0) => {
                self.rtl.finish(f);
                self.rtl = Rtl::default();
                self.mode = Mode::Pcl;
                self.wrapped = true;
            }
            // Any other escape ends PJL.
            _ if self.mode != Mode::Hpgl => {
                self.mode = Mode::Pcl;
                self.rtl.command(&e, f);
            }
            _ => {}
        }
    }

    /// Reads PJL lines from `i` up to `data.len()`; `@PJL ENTER LANGUAGE` switches to HP-GL/2 or
    /// PCL, and any other text ends PJL. Returns where to continue.
    fn pjl(&mut self, data: &[u8], mut i: usize) -> usize {
        while i < data.len() {
            let end = data[i..].iter().position(|&b| b == b'\n').map_or(data.len(), |n| i + n + 1);
            let line = String::from_utf8_lossy(&data[i..end]).trim().to_ascii_uppercase();
            if line.is_empty() {
                i = end;
                continue;
            }
            if !line.starts_with("@PJL") {
                self.mode = Mode::Pcl;
                return i;
            }
            i = end;
            if line.contains("ENTER") && line.contains("LANGUAGE") {
                self.mode = if line.contains("HPGL") { Mode::Hpgl } else { Mode::Pcl };
                return i;
            }
        }
        i
    }
}

#[derive(Clone)]
enum Format {
    Mono,
    Indexed { bits: u32, palette: Vec<u32> },
    /// 8 bits per primary.
    Direct,
}

/// RTL raster state.
struct Rtl {
    dpi: f64,
    /// Cursor units per inch (`ESC&u#D`); raster resolution if unset.
    unit: Option<f64>,
    /// Cursor position in inches.
    cursor: [f64; 2],
    /// Source width in pixels (`ESC*r#S`); 0 = as wide as the widest row.
    width: u32,
    compression: u8,
    format: Result<Format, &'static str>,
    /// Primaries for the next `ESC*v#I`.
    primaries: [f64; 3],
    raster: Option<Raster>,
}

impl Default for Rtl {
    fn default() -> Self {
        Rtl { dpi: 300.0, unit: None, cursor: [0.0; 2], width: 0, compression: 0, format: Ok(Format::Mono), primaries: [0.0; 3], raster: None }
    }
}

/// A raster image being received.
struct Raster {
    x: f64,
    y: f64,
    format: Result<Format, &'static str>,
    rows: Vec<Vec<u8>>,
    /// Last row, the base for delta-row compression.
    seed: Vec<u8>,
}

impl Rtl {
    fn command<'a>(&mut self, e: &Esc, f: &mut impl FnMut(Event<'a>)) {
        let unit = self.unit.unwrap_or(self.dpi);
        match (e.param, e.group, e.term) {
            (b'*', b't', b'R') if e.value > 0.0 => self.dpi = e.value,
            (b'&', b'u', b'D') if e.value > 0.0 => self.unit = Some(e.value),
            (b'*', b'p', axis @ (b'X' | b'Y')) => {
                let k = (axis == b'Y') as usize;
                let v = e.value / unit;
                self.cursor[k] = if e.signed { self.cursor[k] + v } else { v };
            }
            (b'*', b'r', b'S') => self.width = e.value.max(0.0) as u32,
            (b'*', b'r', b'A') => {
                self.finish(f);
                self.start(if e.value == 1.0 { self.cursor[0] } else { 0.0 });
            }
            (b'*', b'r', b'B' | b'C') => self.finish(f),
            (b'*', b'r', b'U') => self.format = if e.value == 1.0 { Ok(Format::Mono) } else { Err("RTL planar colour") },
            (b'*', b'b', b'M') => self.compression = e.value as u8,
            (b'*', b'b', b'W' | b'V') => {
                if self.raster.is_none() {
                    self.start(self.cursor[0]);
                }
                let r = self.raster.as_mut().unwrap();
                if e.term == b'V' {
                    r.format = Err("RTL planar colour");
                }
                if r.format.is_err() {
                    return;
                }
                match row(self.compression, e.data, &mut r.seed) {
                    Ok(()) if r.rows.len() < MAX_ROWS => r.rows.push(r.seed.clone()),
                    Ok(()) => r.format = Err("RTL image too large"),
                    Err(why) => r.format = Err(why),
                }
            }
            (b'*', b'b', b'Y') => match self.raster.as_mut() {
                Some(r) => {
                    let n = e.value.max(0.0) as usize;
                    if r.rows.len() + n > MAX_ROWS {
                        r.format = Err("RTL image too large");
                    } else if r.format.is_ok() {
                        r.seed.fill(0);
                        r.rows.extend(std::iter::repeat_n(Vec::new(), n));
                    }
                }
                None => self.cursor[1] += e.value / self.dpi,
            },
            (b'*', b'v', b'W') => self.format = configure(e.data),
            (b'*', b'v', c @ (b'A' | b'B' | b'C')) => self.primaries[(c - b'A') as usize] = e.value,
            (b'*', b'v', b'I') => {
                if let Ok(Format::Indexed { palette, .. }) = &mut self.format {
                    let [r, g, b] = self.primaries.map(|v| v.clamp(0.0, 255.0) as u32);
                    if let Some(entry) = palette.get_mut(e.value.max(0.0) as usize) {
                        *entry = r << 16 | g << 8 | b;
                    }
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, x: f64) {
        self.raster = Some(Raster { x, y: self.cursor[1], format: self.format.clone(), rows: Vec::new(), seed: Vec::new() });
    }

    /// Ends the raster image being received, if any, and moves the cursor below it.
    fn finish<'a>(&mut self, f: &mut impl FnMut(Event<'a>)) {
        let Some(r) = self.raster.take() else { return };
        self.cursor[1] = r.y + r.rows.len() as f64 / self.dpi;
        let format = match r.format {
            Err(why) => return f(Event::Skip(why)),
            Ok(format) => format,
        };
        let bits = match &format {
            Format::Mono => 1,
            Format::Indexed { bits, .. } => *bits,
            Format::Direct => 24,
        };
        let height = r.rows.len() as u32;
        let width = if self.width > 0 { self.width } else { (r.rows.iter().map(Vec::len).max().unwrap_or(0) as u64 * 8 / bits as u64) as u32 };
        if width == 0 || height == 0 {
            return;
        }
        let pixels = width as u64 * height as u64;
        let image = match format {
            Format::Indexed { bits: 1, palette } if palette[..2] == PALETTE[..2] => bilevel(&r.rows, width, pixels),
            Format::Mono => bilevel(&r.rows, width, pixels),
            _ if pixels > MAX_PIXELS => None,
            format => Some(color(&r.rows, width, bits, &format)),
        };
        f(match image {
            Some(image) => Event::Image(RtlImage { image, x: r.x, y: r.y, dpi: self.dpi }),
            None => Event::Skip("RTL image too large"),
        })
    }
}

/// Pixel format from a Configure Image Data (`ESC*v#W`) block: colour space, pixel encoding mode,
/// bits per index, bits per primary × 3.
fn configure(cid: &[u8]) -> Result<Format, &'static str> {
    let &[_, mode, bits, ..] = cid else { return Err("RTL colour configuration") };
    let bits = bits as u32;
    match mode {
        // Indexed by plane: a single plane is laid out like indexed by pixel.
        0 if bits == 1 => Ok(Format::Indexed { bits, palette: palette(bits) }),
        1 if matches!(bits, 1 | 2 | 4 | 8) => Ok(Format::Indexed { bits, palette: palette(bits) }),
        3 if cid.get(3..6) == Some(&[8, 8, 8]) => Ok(Format::Direct),
        0 | 2 => Err("RTL planar colour"),
        _ => Err("RTL pixel depth"),
    }
}

fn palette(bits: u32) -> Vec<u32> {
    (0..1usize << bits).map(|i| PALETTE.get(i).copied().unwrap_or(0)).collect()
}

/// Decodes one row with compression `mode` into `seed`, which holds the previous row.
fn row(mode: u8, data: &[u8], seed: &mut Vec<u8>) -> Result<(), &'static str> {
    match mode {
        0 => *seed = data.to_vec(),
        1 => *seed = data.chunks_exact(2).flat_map(|p| std::iter::repeat_n(p[1], p[0] as usize + 1)).collect(),
        2 => {
            seed.clear();
            let mut i = 0;
            while i < data.len() {
                let n = data[i] as i8;
                i += 1;
                match n {
                    0.. => {
                        let end = (i + n as usize + 1).min(data.len());
                        seed.extend_from_slice(&data[i..end]);
                        i = end;
                    }
                    -127..=-1 => {
                        if let Some(&b) = data.get(i) {
                            seed.extend(std::iter::repeat_n(b, (1 - n as isize) as usize));
                        }
                        i += 1;
                    }
                    _ => {}
                }
            }
        }
        // Delta row: each command replaces 1–8 bytes at an offset from the end of the previous replacement.
        3 => {
            let (mut i, mut pos) = (0, 0usize);
            while i < data.len() {
                let cmd = data[i];
                i += 1;
                let count = (cmd >> 5) as usize + 1;
                let mut offset = (cmd & 31) as usize;
                if offset == 31 {
                    while let Some(&b) = data.get(i) {
                        i += 1;
                        offset += b as usize;
                        if b != 255 {
                            break;
                        }
                    }
                }
                pos += offset;
                let bytes = &data[i.min(data.len())..(i + count).min(data.len())];
                i += count;
                if seed.len() < pos + bytes.len() {
                    seed.resize(pos + bytes.len(), 0);
                }
                seed[pos..pos + bytes.len()].copy_from_slice(bytes);
                pos += bytes.len();
            }
        }
        _ => return Err("RTL compression (other than 0–3)"),
    }
    Ok(())
}

fn bilevel(rows: &[Vec<u8>], width: u32, pixels: u64) -> Option<Image> {
    if pixels > MAX_BILEVEL_PIXELS {
        return None;
    }
    let mut bmp = Bitmap::new(width, rows.len() as u32);
    let bytes = width.div_ceil(8) as usize;
    for (y, row) in rows.iter().enumerate() {
        let words = bmp.row_mut(y as u32);
        // RTL rows are MSB first; bitmap words hold pixel x at bit x % 64.
        for (k, &b) in row.iter().take(bytes).enumerate() {
            words[k / 8] |= (b.reverse_bits() as u64) << (8 * (k % 8));
        }
        if !width.is_multiple_of(64) {
            *words.last_mut().unwrap() &= (1 << (width % 64)) - 1;
        }
    }
    Some(Image::Bilevel(bmp))
}

fn color(rows: &[Vec<u8>], width: u32, bits: u32, format: &Format) -> Image {
    let mut data = Vec::with_capacity(width as usize * rows.len());
    for row in rows {
        let byte = |k: usize| row.get(k).copied().unwrap_or(0);
        for x in 0..width as usize {
            let c = match format {
                Format::Indexed { palette, .. } => {
                    let bit = x * bits as usize;
                    let v = (byte(bit / 8) as u32) >> (8 - bits - (bit % 8) as u32) & ((1 << bits) - 1);
                    palette[v as usize]
                }
                // An empty row (`ESC*b0W` or a Y offset) is white.
                _ if row.is_empty() => WHITE,
                _ => (byte(3 * x) as u32) << 16 | (byte(3 * x + 1) as u32) << 8 | byte(3 * x + 2) as u32,
            };
            data.push(rgb((c >> 16) as u8, (c >> 8) as u8, c as u8));
        }
    }
    Image::Color(RgbaImage { width, height: rows.len() as u32, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(data: &[u8]) -> (Vec<String>, Vec<RtlImage>) {
        let (mut hpgl, mut images) = (Vec::new(), Vec::new());
        split(data, |e| match e {
            Event::Hpgl(h) => hpgl.push(String::from_utf8_lossy(h).into_owned()),
            Event::Image(i) => images.push(i),
            Event::Skip(why) => hpgl.push(format!("skip {why}")),
        });
        (hpgl, images)
    }

    #[test]
    fn plain_hpgl() {
        assert_eq!(events(b"\x1b.(IN;PD;\x1b%-12345X@PJL EOJ\r\n").0, ["\x1b.(IN;PD;"]);
    }

    #[test]
    fn pjl_and_pcl_wrapper() {
        let data = b"\x1b%-12345X@PJL JOB\r\n@PJL ENTER LANGUAGE=HPGL2\r\nIN;PD1,1;\x1b%-12345X@PJL EOJ\r\n\x1bE\x1b%1BSP1;\x1b%0A\x1b*p0Ytext\x1b%1BPU;";
        assert_eq!(events(data).0, ["IN;PD1,1;", "SP1;", "PU;"]);
    }

    #[test]
    fn rows() {
        let mut seed = Vec::new();
        row(0, &[1, 2, 3], &mut seed).unwrap();
        assert_eq!(seed, [1, 2, 3]);
        row(1, &[2, 7, 0, 9], &mut seed).unwrap();
        assert_eq!(seed, [7, 7, 7, 9]);
        row(2, &[1, 4, 5, 0xFE, 6], &mut seed).unwrap();
        assert_eq!(seed, [4, 5, 6, 6, 6]);
        // Replace 2 bytes at offset 1, then 1 byte 1 further on.
        row(3, &[0b001_00001, 8, 8, 0b000_00001, 9], &mut seed).unwrap();
        assert_eq!(seed, [4, 8, 8, 6, 9]);
        row(3, &[], &mut seed).unwrap();
        assert_eq!(seed, [4, 8, 8, 6, 9]);
        assert!(row(9, &[], &mut seed).is_err());
    }

    #[test]
    fn mono_raster() {
        // 200 dpi, cursor at 100 units of 1/100 inch, 10 px wide, two rows (mode 2), one blank.
        let data = b"\x1bE\x1b*t200R\x1b&u100D\x1b*p100x50Y\x1b*r10S\x1b*r1A\x1b*b2M\x1b*b3W\x01\x80\x40\x1b*b1Y\x1b*b3W\x01\xFF\xC0\x1b*rC";
        let (_, images) = events(data);
        let [img] = &images[..] else { panic!("one image") };
        assert_eq!((img.x, img.y, img.dpi), (1.0, 0.5, 200.0));
        let Image::Bilevel(b) = &img.image else { panic!("bilevel") };
        assert_eq!((b.width, b.height), (10, 3));
        assert!(b.get(0, 0) && b.get(9, 0) && !b.get(1, 0));
        assert!(!(0..10).any(|x| b.get(x, 1)));
        assert!((0..10).all(|x| b.get(x, 2)));
    }

    #[test]
    fn indexed_and_direct_raster() {
        // 8-bit indexed by pixel, entry 5 set to (10, 20, 30).
        let data = b"\x1bE\x1b*v6W\x00\x01\x08\x08\x08\x08\x1b*v10a20b30c5I\x1b*r1A\x1b*b2W\x05\x01\x1b*rC\x1b*v6W\x00\x03\x00\x08\x08\x08\x1b*r1A\x1b*b3W\x01\x02\x03\x1b*rC";
        let (_, images) = events(data);
        let [a, b] = &images[..] else { panic!("two images") };
        let Image::Color(a) = &a.image else { panic!("colour") };
        assert_eq!(a.data, [rgb(10, 20, 30), rgb(0, 0, 0)]);
        let Image::Color(b) = &b.image else { panic!("colour") };
        assert_eq!(b.data, [rgb(1, 2, 3)]);
    }
}
