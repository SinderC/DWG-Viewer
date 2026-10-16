//! The clear-text encoding (ISO 8632-4), transcoded to the binary encoding so one interpreter draws both.
//!
//! The binary stream is written at fixed precisions: 32-bit integers, indices and colours, and 64-bit
//! floating-point reals and VDC. Elements the interpreter ignores are dropped.

/// Whether `data` starts, after blanks and comments, with BEGIN METAFILE.
pub(super) fn is_text(data: &[u8]) -> bool {
    let mut lexer = Lexer { data, pos: 0 };
    matches!(lexer.next(), Some(Ok(Tok::Atom(w))) if w == "BEGMF")
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A name or number, upper-cased, without the null characters `_` and `$`. Numbers are parsed
    /// when read, so a bitstream's hexadecimal digits keep their leading zeros.
    Atom(String),
    Str(Vec<u8>),
    /// `;` or `/`.
    End,
}

/// Splits clear text into tokens; commas, parentheses and `%…%` comments separate them.
struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

fn atom_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'+' | b'-' | b'.' | b'#')
}

impl Iterator for Lexer<'_> {
    type Item = Result<Tok, String>;

    fn next(&mut self) -> Option<Self::Item> {
        let d = self.data;
        loop {
            let &c = d.get(self.pos)?;
            match c {
                b'%' => match d[self.pos + 1..].iter().position(|&b| b == b'%') {
                    Some(n) => self.pos += n + 2,
                    None => return Some(Err("unterminated comment".into())),
                },
                c if c.is_ascii_whitespace() || matches!(c, b',' | b'(' | b')') => self.pos += 1,
                _ => break,
            }
        }
        let start = self.pos;
        let c = d[start];
        self.pos += 1;
        Some(Ok(match c {
            b';' | b'/' => Tok::End,
            b'\'' | b'"' => {
                // A doubled quote stands for itself.
                let mut s = Vec::new();
                loop {
                    match d.get(self.pos) {
                        None => return Some(Err("unterminated string".into())),
                        Some(&q) if q == c && d.get(self.pos + 1) == Some(&c) => {
                            s.push(c);
                            self.pos += 2;
                        }
                        Some(&q) if q == c => {
                            self.pos += 1;
                            break Tok::Str(s);
                        }
                        Some(&b) => {
                            s.push(b);
                            self.pos += 1;
                        }
                    }
                }
            }
            c if atom_char(c) => {
                while d.get(self.pos).is_some_and(|&b| atom_char(b)) {
                    self.pos += 1;
                }
                Tok::Atom(d[start..self.pos].iter().filter(|b| !matches!(b, b'_' | b'$')).map(|b| b.to_ascii_uppercase() as char).collect())
            }
            c => return Some(Err(format!("unexpected character '{}'", c as char))),
        }))
    }
}

/// A decimal or real number, or a based integer such as `16#FF` or `-2#101`.
fn number(s: &str) -> Result<f64, String> {
    let bad = || format!("expected a number, found {s}");
    if !s.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '.')) {
        return Err(bad());
    }
    let Some((base, digits)) = s.split_once('#') else { return s.parse().map_err(|_| bad()) };
    let (sign, base) = match base.strip_prefix('-') {
        Some(b) => (-1.0, b),
        None => (1.0, base.trim_start_matches('+')),
    };
    let base = base.parse().ok().filter(|b| (2..=16).contains(b)).ok_or_else(bad)?;
    Ok(sign * i64::from_str_radix(digits, base).map_err(|_| bad())? as f64)
}

/// Parameter types.
#[derive(Clone, Copy)]
enum T {
    Int,
    Real,
    /// A VDC value; a point is two.
    Vdc,
    Str,
    /// Keywords in value order (`|` separates spellings); a number is also accepted.
    Enum(&'static [&'static str]),
    /// Indexed or direct, by the colour selection mode.
    Colour,
    Direct,
}

use T::*;

const P: [T; 2] = [Vdc, Vdc];
const FINAL: T = Enum(&["NOTFINAL", "FINAL"]);
const CLOSE: T = Enum(&["PIE", "CHORD"]);
const WIDTH_MODE: T = Enum(&["ABS|ABSTRACT", "SCALED", "FRACTIONAL", "MM"]);

/// Clear-text name, binary class and id, parameters, and how many of the last parameters repeat.
type Element = (&'static str, u8, u8, &'static [T], usize);

const ELEMENTS: &[Element] = &[
    ("ENDMF", 0, 2, &[], 0),
    ("BEGPIC", 0, 3, &[Str], 0),
    ("BEGPICBODY", 0, 4, &[], 0),
    ("ENDPIC", 0, 5, &[], 0),
    ("BEGFIGURE", 0, 8, &[], 0),
    ("ENDTILEARRAY", 0, 20, &[], 0),
    ("BEGAPSBODY", 0, 22, &[], 0),
    ("ENDAPS", 0, 23, &[], 0),
    ("MFVERSION", 1, 1, &[Int], 0),
    ("MFDESC", 1, 2, &[Str], 0),
    ("COLRVALUEEXT", 1, 10, &[Direct, Direct], 0),
    ("FONTLIST", 1, 13, &[Str], 1),
    ("SCALEMODE", 2, 1, &[Enum(&["ABSTRACT", "METRIC"]), Real], 0),
    ("COLRMODE", 2, 2, &[Enum(&["INDEXED", "DIRECT"])], 0),
    ("LINEWIDTHMODE", 2, 3, &[WIDTH_MODE], 0),
    ("MARKERSIZEMODE", 2, 4, &[WIDTH_MODE], 0),
    ("EDGEWIDTHMODE", 2, 5, &[WIDTH_MODE], 0),
    ("VDCEXT", 2, 6, &[Vdc, Vdc, Vdc, Vdc], 0),
    ("BACKCOLR", 2, 7, &[Direct], 0),
    ("LINE", 4, 1, &P, 2),
    ("DISJTLINE", 4, 2, &P, 2),
    ("MARKER", 4, 3, &P, 2),
    ("TEXT", 4, 4, &[Vdc, Vdc, FINAL, Str], 0),
    ("RESTRTEXT", 4, 5, &[Vdc, Vdc, Vdc, Vdc, FINAL, Str], 0),
    ("APNDTEXT", 4, 6, &[FINAL, Str], 0),
    ("POLYGON", 4, 7, &P, 2),
    ("POLYGONSET", 4, 8, &[Vdc, Vdc, Enum(&["INVIS", "VIS", "CLOSEINVIS", "CLOSEVIS"])], 3),
    ("GDP", 4, 10, &[], 0),
    ("RECT", 4, 11, &[Vdc, Vdc, Vdc, Vdc], 0),
    ("CIRCLE", 4, 12, &[Vdc, Vdc, Vdc], 0),
    ("ARC3PT", 4, 13, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc], 0),
    ("ARC3PTCLOSE", 4, 14, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, CLOSE], 0),
    ("ARCCTR", 4, 15, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc], 0),
    ("ARCCTRCLOSE", 4, 16, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, CLOSE], 0),
    ("ELLIPSE", 4, 17, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc], 0),
    ("ELLIPARC", 4, 18, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc], 0),
    ("ELLIPARCCLOSE", 4, 19, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, CLOSE], 0),
    ("ARCCTRREV", 4, 20, &[Vdc, Vdc, Vdc, Vdc, Vdc, Vdc, Vdc], 0),
    ("CONNEDGE", 4, 21, &[], 0),
    ("HYPERBARC", 4, 22, &[], 0),
    ("PARABARC", 4, 23, &[], 0),
    ("NUB", 4, 24, &[], 0),
    ("NURB", 4, 25, &[], 0),
    ("POLYBEZIER", 4, 26, &[Int, Vdc, Vdc], 2),
    ("POLYSYMBOL", 4, 27, &[], 0),
    ("LINETYPE", 5, 2, &[Int], 0),
    // Widths and sizes are reals or VDC by the specification mode; both are written as 64-bit floats.
    ("LINEWIDTH", 5, 3, &[Real], 0),
    ("LINECOLR", 5, 4, &[Colour], 0),
    ("MARKERTYPE", 5, 6, &[Int], 0),
    ("MARKERSIZE", 5, 7, &[Real], 0),
    ("MARKERCOLR", 5, 8, &[Colour], 0),
    ("CHAREXPAN", 5, 12, &[Real], 0),
    ("CHARSPACE", 5, 13, &[Real], 0),
    ("TEXTCOLR", 5, 14, &[Colour], 0),
    ("CHARHEIGHT", 5, 15, &[Vdc], 0),
    ("CHARORI", 5, 16, &[Vdc, Vdc, Vdc, Vdc], 0),
    ("TEXTPATH", 5, 17, &[Enum(&["RIGHT", "LEFT", "UP", "DOWN"])], 0),
    (
        "TEXTALIGN",
        5,
        18,
        &[
            Enum(&["NORMHORIZ", "LEFT", "CTR", "RIGHT", "CONTHORIZ"]),
            Enum(&["NORMVERT", "TOP", "CAP", "HALF", "BASE", "BOTTOM", "CONTVERT"]),
            Real,
            Real,
        ],
        0,
    ),
    ("INTSTYLE", 5, 22, &[Enum(&["HOLLOW", "SOLID", "PAT", "HATCH", "EMPTY", "GEOPAT", "INTERP"])], 0),
    ("FILLCOLR", 5, 23, &[Colour], 0),
    ("HATCHINDEX", 5, 24, &[Int], 0),
    ("EDGETYPE", 5, 27, &[Int], 0),
    ("EDGEWIDTH", 5, 28, &[Real], 0),
    ("EDGECOLR", 5, 29, &[Colour], 0),
    ("EDGEVIS", 5, 30, &[Enum(&["OFF", "ON"])], 0),
    ("COLRTABLE", 5, 34, &[Int, Direct], 1),
];

/// Writes one binary element, in partitions if long.
fn element(out: &mut Vec<u8>, class: u8, id: u8, data: &[u8]) {
    let head = (class as u16) << 12 | (id as u16) << 5;
    let pad = |out: &mut Vec<u8>, part: &[u8]| {
        out.extend(part);
        if part.len() % 2 == 1 {
            out.push(0);
        }
    };
    if data.len() < 31 {
        out.extend((head | data.len() as u16).to_be_bytes());
        return pad(out, data);
    }
    out.extend((head | 31).to_be_bytes());
    let mut parts = data.chunks(32766).peekable();
    while let Some(part) = parts.next() {
        let more = if parts.peek().is_some() { 0x8000 } else { 0 };
        out.extend((more | part.len() as u16).to_be_bytes());
        pad(out, part);
    }
}

fn string(out: &mut Vec<u8>, s: &[u8]) {
    if s.len() < 255 {
        out.push(s.len() as u8);
        return out.extend(s);
    }
    out.push(255);
    let mut parts = s.chunks(32767).peekable();
    while let Some(part) = parts.next() {
        let more = if parts.peek().is_some() { 0x8000 } else { 0 };
        out.extend((more | part.len() as u16).to_be_bytes());
        out.extend(part);
    }
}

/// The colour selection mode and the number of direct colour components.
#[derive(Clone, Copy)]
struct Mode {
    direct: bool,
    components: usize,
}

impl Default for Mode {
    fn default() -> Self {
        Mode { direct: false, components: 3 }
    }
}

/// The parameters of one element.
struct Args<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Args<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Args { toks, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn next(&mut self) -> Result<&'a Tok, String> {
        let t = self.toks.get(self.pos).ok_or("missing parameters")?;
        self.pos += 1;
        Ok(t)
    }

    fn atom(&mut self) -> Result<&'a str, String> {
        match self.next()? {
            Tok::Atom(a) => Ok(a),
            t => Err(format!("expected a number or name, found {t:?}")),
        }
    }

    fn num(&mut self) -> Result<f64, String> {
        number(self.atom()?)
    }

    fn int(&mut self) -> Result<i32, String> {
        Ok(self.num()?.round() as i32)
    }

    fn str(&mut self) -> Result<&'a [u8], String> {
        match self.next()? {
            Tok::Str(s) => Ok(s),
            t => Err(format!("expected a string, found {t:?}")),
        }
    }

    /// A keyword from `names` (by position) or a number.
    fn keyword(&mut self, names: &[&str]) -> Result<i32, String> {
        let a = self.atom()?;
        match names.iter().position(|n| n.split('|').any(|n| n == a)) {
            Some(v) => Ok(v as i32),
            None => number(a).map(|v| v as i32).map_err(|_| format!("unknown value {a}")),
        }
    }

    /// The remaining parameters as a bitstream of hexadecimal digits.
    fn bitstream(&mut self) -> Result<Vec<u8>, String> {
        let mut digits = Vec::new();
        while !self.is_empty() {
            for c in self.atom()?.chars() {
                digits.push(c.to_digit(16).ok_or_else(|| format!("bad bitstream digit {c}"))? as u8);
            }
        }
        // An odd final digit fills the high half of the last byte.
        Ok(digits.chunks(2).map(|d| d[0] << 4 | d.get(1).copied().unwrap_or(0)).collect())
    }

    fn write(&mut self, t: T, mode: Mode, out: &mut Vec<u8>) -> Result<(), String> {
        match t {
            Int => out.extend(self.int()?.to_be_bytes()),
            Real | Vdc => out.extend(self.num()?.to_be_bytes()),
            Str => string(out, self.str()?),
            Enum(names) => out.extend((self.keyword(names)? as i16).to_be_bytes()),
            Colour if !mode.direct => return self.write(Int, mode, out),
            Colour | Direct => {
                for _ in 0..mode.components {
                    out.extend(self.int()?.to_be_bytes());
                }
            }
        }
        Ok(())
    }
}

/// A structured data record: members of a type, a count and that many values, written in the
/// binary encoding at the precisions used here. Nested records and colour lists are not supported.
fn sdr(text: &[u8], mode: Mode) -> Result<Vec<u8>, String> {
    let toks = (Lexer { data: text, pos: 0 }).collect::<Result<Vec<_>, _>>()?;
    let (args, mut out) = (&mut Args::new(&toks), Vec::new());
    while !args.is_empty() {
        let (kind, count) = (args.int()?, args.int()?);
        out.extend(kind.to_be_bytes());
        out.extend(count.to_be_bytes());
        for _ in 0..count {
            match kind {
                // Colour direct
                3 => args.write(Direct, mode, &mut out)?,
                // Name and enumerated (16-bit), 8- and 16-bit integers
                4 | 5 | 9 | 22 => out.extend((args.int()? as i16).to_be_bytes()),
                8 | 18 => out.push(args.int()? as u8),
                // Real, VDC and viewport coordinates
                12 | 15 | 16 => out.extend(args.num()?.to_be_bytes()),
                13 | 14 => string(&mut out, args.str()?),
                20 => string(&mut out, &args.bitstream()?),
                1 | 21 => return Err(format!("structured data of type {kind} is not supported")),
                // Indices, colour indices and components, 32-bit integers
                _ => out.extend(args.int()?.to_be_bytes()),
            }
        }
    }
    Ok(out)
}

/// Transcodes clear text to a binary metafile; the second value is the first problem met, if any.
pub(super) fn transcode(data: &[u8]) -> (Vec<u8>, Option<String>) {
    let mut t = Transcoder::default();
    let mut warning = None;
    let mut toks = Vec::new();
    for tok in (Lexer { data, pos: 0 }) {
        match tok {
            Ok(Tok::End) => {}
            Ok(tok) => {
                toks.push(tok);
                continue;
            }
            Err(e) => {
                warning.get_or_insert(e);
                break;
            }
        }
        match toks.split_first() {
            Some((Tok::Atom(name), args)) => {
                if let Err(e) = t.element(name, &mut Args::new(args)) {
                    warning.get_or_insert(format!("{name} ({e})"));
                }
            }
            _ => {
                warning.get_or_insert("an element does not start with its name".into());
            }
        }
        toks.clear();
    }
    (t.out, warning)
}

#[derive(Default)]
struct Transcoder {
    out: Vec<u8>,
    /// Elements between BEGIN and END METAFILE DEFAULTS, written as METAFILE DEFAULTS REPLACEMENT.
    defaults: Option<Vec<u8>>,
    /// The current picture's mode and the metafile default.
    mode: Mode,
    default_mode: Mode,
    /// COLOUR PRECISION: the largest colour component value.
    colour_max: Option<i32>,
}

impl Transcoder {
    fn emit(&mut self, class: u8, id: u8, data: &[u8]) {
        element(self.defaults.as_mut().unwrap_or(&mut self.out), class, id, data);
    }

    /// METAFILE DEFAULTS REPLACEMENT holding one element.
    fn replace_default(&mut self, class: u8, id: u8, data: &[u8]) {
        let mut replacement = Vec::new();
        element(&mut replacement, class, id, data);
        self.emit(1, 12, &replacement);
    }

    /// COLOUR VALUE EXTENT from 0 to the colour precision, for the current number of components.
    fn colour_extent(&mut self) {
        let Some(max) = self.colour_max else { return };
        let n = self.mode.components;
        let data: Vec<u8> = std::iter::repeat_n(0, n).chain(std::iter::repeat_n(max, n)).flat_map(i32::to_be_bytes).collect();
        self.emit(1, 10, &data);
    }

    fn element(&mut self, name: &str, args: &mut Args) -> Result<(), String> {
        let mut data = Vec::new();
        let mode = self.mode;
        match name {
            "BEGMF" => {
                args.write(Str, mode, &mut data)?;
                self.emit(0, 1, &data);
                // INTEGER PRECISION is read at the default 16 bits; the rest at 32.
                self.emit(1, 4, &32i16.to_be_bytes());
                for id in [6, 7, 8] {
                    self.emit(1, id, &32i32.to_be_bytes());
                }
                let float64 = [&0i16.to_be_bytes()[..], &12i32.to_be_bytes(), &52i32.to_be_bytes()].concat();
                self.emit(1, 5, &float64);
                self.replace_default(3, 2, &float64);
                self.emit(1, 3, &1i16.to_be_bytes());
            }
            // VDC are always written as reals; integer VDC differ only in their default extent.
            "VDCTYPE" => {
                if args.keyword(&["INTEGER", "REAL"])? == 0 {
                    let extent: Vec<u8> = [0.0f64, 0.0, 32767.0, 32767.0].iter().flat_map(|v| v.to_be_bytes()).collect();
                    self.replace_default(2, 6, &extent);
                }
            }
            // The largest colour component value, which sets the default colour value extent.
            "COLRPREC" => {
                self.colour_max = Some(args.int()?);
                self.colour_extent();
            }
            "COLRMODEL" => {
                let model = args.keyword(&["", "RGB", "CIELAB", "CIELUV", "CMYK", "RGBRELATED"])?;
                self.emit(1, 19, &model.to_be_bytes());
                self.mode.components = if model == 4 { 4 } else { 3 };
                self.default_mode.components = self.mode.components;
                self.colour_extent();
            }
            "BEGMFDEFAULTS" => self.defaults = Some(Vec::new()),
            "ENDMFDEFAULTS" => {
                let replacement = self.defaults.take().unwrap_or_default();
                self.emit(1, 12, &replacement);
            }
            // Corners, cell counts and the colour precision (ignored), then the cells row by row.
            "CELLARRAY" => {
                for _ in 0..6 {
                    args.write(Vdc, mode, &mut data)?;
                }
                let (nx, ny) = (args.int()?, args.int()?);
                args.int()?;
                // Packed at the full colour precision.
                data.extend([nx, ny, 0].iter().flat_map(|v| v.to_be_bytes()));
                data.extend(1i16.to_be_bytes());
                for _ in 0..nx.max(0) as u64 * ny.max(0) as u64 {
                    args.write(Colour, mode, &mut data)?;
                }
                self.emit(4, 9, &data);
            }
            // Identifier, type and inheritance flag (which the interpreter does not use).
            "BEGAPS" => {
                string(&mut data, args.str()?);
                string(&mut data, args.str()?);
                data.extend(0i16.to_be_bytes());
                self.emit(0, 21, &data);
            }
            "APSATTR" => {
                string(&mut data, args.str()?);
                string(&mut data, &sdr(args.str()?, mode)?);
                self.emit(9, 1, &data);
            }
            // Position; cell path (0°, 90°, 180° or 270°) and line progression (90° or 270°)
            // directions, as degrees or their binary values; tile and cell counts; cell sizes; image offsets and size.
            "BEGTILEARRAY" => {
                args.write(Vdc, mode, &mut data)?;
                args.write(Vdc, mode, &mut data)?;
                let path = args.int()?;
                let line = args.int()?;
                data.extend((if path >= 90 { path / 90 } else { path } as i16).to_be_bytes());
                data.extend((if line >= 90 { (line == 270) as i32 } else { line } as i16).to_be_bytes());
                for t in [Int, Int, Int, Int, Real, Real, Int, Int, Int, Int] {
                    args.write(t, mode, &mut data)?;
                }
                self.emit(0, 19, &data);
            }
            // Compression, row padding, the cell colour precision or the two colours of a bitonal
            // tile, method parameters (a structured data record) and the bitstream.
            "TILE" | "BITONALTILE" => {
                args.write(Int, mode, &mut data)?;
                args.write(Int, mode, &mut data)?;
                if name == "TILE" {
                    args.write(Int, mode, &mut data)?;
                } else {
                    args.write(Colour, mode, &mut data)?;
                    args.write(Colour, mode, &mut data)?;
                }
                string(&mut data, &sdr(args.str()?, mode)?);
                data.extend(args.bitstream()?);
                self.emit(4, if name == "TILE" { 29 } else { 28 }, &data);
            }
            _ => {
                let Some(&(_, class, id, params, repeat)) = ELEMENTS.iter().find(|e| e.0 == name) else { return Ok(()) };
                for &t in params {
                    args.write(t, mode, &mut data)?;
                }
                while repeat > 0 && !args.is_empty() {
                    for &t in &params[params.len() - repeat..] {
                        args.write(t, mode, &mut data)?;
                    }
                }
                match name {
                    "BEGPIC" => self.mode = self.default_mode,
                    "COLRMODE" => {
                        self.mode.direct = data == 1i16.to_be_bytes();
                        if self.defaults.is_some() {
                            self.default_mode.direct = self.mode.direct;
                        }
                    }
                    _ => {}
                }
                self.emit(class, id, &data);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::BACKGROUND;
    use crate::tiff::Image;

    fn lex(s: &str) -> Vec<Tok> {
        Lexer { data: s.as_bytes(), pos: 0 }.map(Result::unwrap).collect()
    }

    #[test]
    fn tokens() {
        let a = |s: &str| Tok::Atom(s.into());
        assert_eq!(
            lex("Beg_Pic 'It''s' % note % (1,-2.5) $12$3 .9/"),
            [a("BEGPIC"), Tok::Str(b"It's".to_vec()), a("1"), a("-2.5"), a("123"), a(".9"), Tok::End]
        );
        assert!(Lexer { data: b"'open", pos: 0 }.next().unwrap().is_err());
        assert!(is_text(b"\n% header %\n begmf 'x';") && !is_text(b"\0\x22"));
        let numbers: Vec<f64> = ["16#FF", "-16#8000", "2#1111", "42E2", "-.5"].iter().map(|s| number(s).unwrap()).collect();
        assert_eq!(numbers, [255.0, -32768.0, 15.0, 4200.0, -0.5]);
        assert!(number("INF").is_err() && number("17#1").is_err());
    }

    fn decode(body: &str) -> crate::dxf::Drawing {
        let file = format!("BEGMF 'test'; VDCTYPE INTEGER; BEGPIC 'p'; VDCEXT 0 0 100 100; BEGPICBODY; {body} ENDPIC; ENDMF;");
        let (d, pages) = super::super::decode(file.as_bytes(), 0).unwrap();
        assert_eq!(pages, 1);
        assert!(!d.info.iter().any(|(k, _)| k == "Warning"), "{:?}", d.info);
        d
    }

    #[test]
    fn primitives_and_colours() {
        let d = decode(
            "COLRTABLE 2 255 0 0 0 255 0; LINECOLR 2; LINE (0,0) (10,10) (20,0);
             COLRMODE DIRECT; FILLCOLR 255 255 255; INTSTYLE SOLID; RECT 0 0 10 10;
             TEXTALIGN CTR HALF 0 0; TEXT (5,5) FINAL \"Hi\";
             POLYGONSET (0,0) VIS (10,0) VIS (10,10) CLOSEINVIS;",
        );
        assert_eq!((d.paths[0].points.len(), d.paths[0].color), (3, 0xFF0000));
        assert_eq!(d.fills.len(), 2);
        assert_eq!(d.fills[0].color, BACKGROUND);
        assert_eq!((d.texts[0].text.as_str(), d.texts[0].halign, d.texts[0].valign), ("Hi", 1, 2));
        assert_eq!(d.paths.len(), 1);
    }

    #[test]
    fn cell_array_and_defaults() {
        let d = decode("CELLARRAY (0,10) (20,0) (20,10) 2 1 255 (1 0);");
        assert_eq!(d.images.len(), 1);
        // Integer VDC without an extent default to 0..32767.
        let file = "BEGMF 'x'; VDCTYPE INTEGER; BEGMFDEFAULTS; LINECOLR 0; ENDMFDEFAULTS; BEGPIC 'p'; BEGPICBODY; LINE 0 0 32767 0; ENDPIC; ENDMF;";
        let d = super::super::decode(file.as_bytes(), 0).unwrap().0;
        assert_eq!((d.width, d.paths[0].color), (32767.0, BACKGROUND));
    }

    #[test]
    fn bad_elements_are_skipped_with_a_warning() {
        let file = "BEGMF 'x'; BEGPIC 'p'; BEGPICBODY; LINE 0 0 1; INTSTYLE PLAID; LINE 0 0 1 1; ENDPIC; ENDMF;";
        let d = super::super::decode(file.as_bytes(), 0).unwrap().0;
        assert_eq!(d.paths.len(), 1);
        assert!(d.info.contains(&("Warning".into(), "LINE (missing parameters)".into())));
    }

    #[test]
    fn layers_cmyk_and_markers() {
        let d = decode(
            "BEGAPS 'l1' 'layer' STLIST; APSATTR 'layername' \"14 1 'Walls'\"; BEGAPSBODY; LINE 0 0 1 1; ENDAPS;
             MARKERTYPE 2; MARKERSIZE 2.0; MARKER (5,5) (6,6); BACKCOLR 0 0 255;",
        );
        assert_eq!(d.background.as_ref().unwrap().color, 0x0000FF);
        assert_eq!(d.layers[d.paths[0].layer as usize].name, "Walls");
        assert_eq!(d.paths.len(), 5);
        // CMYK: four components; COLRPREC sets the extent of all four.
        let file = "BEGMF 'x'; COLRPREC 100; COLRMODEL 4; BEGPIC 'p'; COLRMODE DIRECT; BEGPICBODY; LINECOLR 100 0 100 0; LINE 0 0 1 1; ENDPIC; ENDMF;";
        let d = super::super::decode(file.as_bytes(), 0).unwrap().0;
        assert_eq!(d.paths[0].color, 0x00FF00);
    }

    #[test]
    fn group4_tile() {
        use fax::{encoder::Encoder, Color, VecWriter};
        let mut enc = Encoder::new(VecWriter::new());
        for y in 0..4 {
            enc.encode_line((0..8).map(|x| if x == y { Color::Black } else { Color::White }), 8).unwrap();
        }
        let hex: String = enc.finish().unwrap().finish().iter().map(|b| format!("{b:02x} ")).collect();
        let d = decode(&format!("BEGTILEARRAY (0,4) 0 270 1 1 8 4 1.0 1.0 0 0 8 4; BITONALTILE 2 0 0 1 '' {hex}; ENDTILEARRAY;"));
        let Image::Bilevel(b) = &d.images[0].image else { panic!("expected bilevel") };
        assert!(b.get(2, 2) && !b.get(3, 2));
    }
}
