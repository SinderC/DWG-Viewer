//! DXF group-code tokenizer and record splitting (ASCII DXF only).

/// One group: an integer code and its raw value (trailing whitespace removed).
#[derive(Clone, Copy, Debug)]
pub struct Pair<'a> {
    pub code: i32,
    pub value: &'a str,
}

/// The groups from one `0` code to the next: `kind` is the `0` value (`LINE`, `SECTION`, …).
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    pub kind: &'a str,
    pub pairs: &'a [Pair<'a>],
}

const BINARY_SENTINEL: &[u8] = b"AutoCAD Binary DXF";

/// Decodes the file as UTF-8 (R2007+) or, failing that, Latin-1 (older files use the ANSI code page).
pub fn decode_text(data: &[u8]) -> Result<String, String> {
    if data.starts_with(BINARY_SENTINEL) {
        return Err("Binary DXF is not supported; save the drawing as ASCII DXF".into());
    }
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    Ok(match std::str::from_utf8(data) {
        Ok(s) => s.to_owned(),
        Err(_) => data.iter().map(|&b| b as char).collect(),
    })
}

pub fn pairs(text: &str) -> Result<Vec<Pair<'_>>, String> {
    let mut lines = text.lines();
    let mut out = Vec::new();
    while let Some(code) = lines.next() {
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let code = code.parse().map_err(|_| format!("Invalid DXF group code {code:?}"))?;
        let value = lines.next().ok_or("Unexpected end of DXF file")?.trim_end();
        out.push(Pair { code, value });
    }
    Ok(out)
}

pub fn records<'a>(pairs: &'a [Pair<'a>]) -> Vec<Record<'a>> {
    let mut out = Vec::new();
    let mut rest = pairs;
    // Anything before the first `0` group is not part of a record.
    while let Some(start) = rest.iter().position(|p| p.code == 0) {
        let body = &rest[start + 1..];
        let end = body.iter().position(|p| p.code == 0).unwrap_or(body.len());
        out.push(Record { kind: rest[start].value.trim(), pairs: &body[..end] });
        rest = &body[end..];
    }
    out
}

impl<'a> Record<'a> {
    pub fn get(&self, code: i32) -> Option<&'a str> {
        self.pairs.iter().find(|p| p.code == code).map(|p| p.value)
    }

    pub fn f(&self, code: i32, default: f64) -> f64 {
        self.get(code).and_then(|v| v.trim().parse().ok()).filter(|v: &f64| v.is_finite()).unwrap_or(default)
    }

    pub fn i(&self, code: i32, default: i64) -> i64 {
        // Some writers emit integers as reals ("1.0").
        self.get(code).and_then(|v| v.trim().parse().ok().or_else(|| v.trim().parse::<f64>().ok().map(|f| f as i64))).unwrap_or(default)
    }

    /// The point in codes `code` (x) and `code + 10` (y).
    pub fn point(&self, code: i32) -> [f64; 2] {
        [self.f(code, 0.0), self.f(code + 10, 0.0)]
    }

    /// Case-insensitive name in code 2 (block and layer names), or "".
    pub fn name(&self, code: i32) -> String {
        self.get(code).unwrap_or("").trim().to_uppercase()
    }
}

/// Replaces `\U+XXXX` escapes (used by pre-2007 files for non-ANSI characters).
pub fn unescape_unicode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\\U+") {
        out.push_str(&rest[..i]);
        let hex = rest.get(i + 3..i + 7);
        match hex.and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32) {
            Some(c) => {
                out.push(c);
                rest = &rest[i + 7..];
            }
            None => {
                out.push_str("\\U+");
                rest = &rest[i + 3..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Resolves TEXT control codes: `%%c` Ø, `%%d` °, `%%p` ±, `%%nnn` character nnn; drops under/overline toggles.
pub fn text_codes(s: &str) -> String {
    let s = unescape_unicode(s);
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find("%%") {
        out.push_str(&rest[..i]);
        rest = &rest[i + 2..];
        let mut chars = rest.chars();
        match chars.next().map(|c| c.to_ascii_lowercase()) {
            Some('c') => out.push('Ø'),
            Some('d') => out.push('°'),
            Some('p') => out.push('±'),
            Some('u' | 'o' | 'k') => {}
            Some('%') => out.push('%'),
            Some(c) if c.is_ascii_digit() => {
                let digits = rest.bytes().take(3).take_while(u8::is_ascii_digit).count();
                out.extend(rest[..digits].parse().ok().and_then(char::from_u32));
                rest = &rest[digits..];
                continue;
            }
            _ => {
                out.push_str("%%");
                continue;
            }
        }
        rest = chars.as_str();
    }
    out.push_str(rest);
    out
}

/// Strips MTEXT inline formatting, keeping the text. `\P` becomes a newline.
pub fn mtext_plain(s: &str) -> String {
    let s = text_codes(s);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' => {}
            '\\' => match chars.next() {
                Some('P' | 'X') => out.push('\n'),
                Some('~') => out.push('\u{a0}'),
                Some(c @ ('\\' | '{' | '}')) => out.push(c),
                // Stacked fraction: `\S1/2;` or `\S1^2;`.
                Some('S') => {
                    for c in chars.by_ref().take_while(|&c| c != ';') {
                        out.push(if c == '^' || c == '#' { '/' } else { c });
                    }
                }
                Some('L' | 'l' | 'O' | 'o' | 'K' | 'k' | 'N') => {}
                // Codes with an argument ending in `;` (font, height, colour, …).
                Some(_) => chars.by_ref().take_while(|&c| c != ';').for_each(drop),
                None => {}
            },
            _ => out.push(c),
        }
    }
    out
}

/// Marks a style change in rich text: `STYLE` + fields + `STYLE_END` (see `RichStyle::encode`).
pub const STYLE: char = '\u{E000}';
pub const STYLE_END: char = '\u{E001}';
/// A stacked fraction: `STACK` top `STACK_OVER` bottom `STACK_KIND` separator (`/`, `#` or `^`) `STACK_END`.
pub const STACK: char = '\u{E002}';
pub const STACK_OVER: char = '\u{E003}';
pub const STACK_KIND: char = '\u{E004}';
pub const STACK_END: char = '\u{E005}';

/// Inline MTEXT formatting that the renderer applies to the text after it.
#[derive(Clone, PartialEq)]
struct RichStyle {
    /// Height and width factors, tracking factor and oblique angle (degrees).
    height: f64,
    width: f64,
    tracking: f64,
    oblique: f64,
    /// 0xRRGGBB, or None for the entity's colour.
    color: Option<u32>,
    /// `dxf::FONT_*` family and style bits.
    font: u8,
    underline: bool,
    overline: bool,
    strike: bool,
}

impl Default for RichStyle {
    fn default() -> Self {
        RichStyle { height: 1.0, width: 1.0, tracking: 1.0, oblique: 0.0, color: None, font: 0, underline: false, overline: false, strike: false }
    }
}

impl RichStyle {
    /// `height;width;tracking;oblique;colour (hex, or - for the entity's);font;lines (1 under, 2 over, 4 strike)`.
    fn encode(&self) -> String {
        let color = self.color.map_or("-".to_string(), |c| format!("{c:06x}"));
        let lines = self.underline as u8 | (self.overline as u8) << 1 | (self.strike as u8) << 2;
        format!("{};{};{};{};{color};{};{lines}", self.height, self.width, self.tracking, self.oblique, self.font)
    }
}

/// MTEXT with its inline formatting as rich-text markup (see `STYLE` and `STACK`) for text of
/// `height`, or plain text (as `mtext_plain`) if it has no formatting that changes how it looks.
pub fn mtext_rich(s: &str, height: f64) -> String {
    let text = text_codes(s);
    let mut out = String::with_capacity(text.len());
    let (mut cur, mut shown, mut saved) = (RichStyle::default(), RichStyle::default(), Vec::new());
    let mut chars = text.chars().peekable();
    // Writes `c` in the current style.
    let mut put = |out: &mut String, cur: &RichStyle, c: &str| {
        if *cur != shown {
            out.push(STYLE);
            out.push_str(&cur.encode());
            out.push(STYLE_END);
            shown = cur.clone();
        }
        out.push_str(c);
    };
    while let Some(c) = chars.next() {
        match c {
            '{' => saved.push(cur.clone()),
            '}' => cur = saved.pop().unwrap_or_default(),
            '\\' => {
                let Some(code) = chars.next() else { break };
                // The argument up to `;`.
                let mut arg = || chars.by_ref().take_while(|&c| c != ';').collect::<String>();
                let number = |a: &str| a.trim_end_matches(['x', 'X']).trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0);
                match code {
                    'P' | 'X' | 'N' => out.push('\n'),
                    '~' => put(&mut out, &cur, "\u{a0}"),
                    '\\' | '{' | '}' => put(&mut out, &cur, code.encode_utf8(&mut [0; 4])),
                    'L' | 'l' => cur.underline = code == 'L',
                    'O' | 'o' => cur.overline = code == 'O',
                    'K' | 'k' => cur.strike = code == 'K',
                    'H' => {
                        let a = arg();
                        if let Some(v) = number(&a) {
                            cur.height = if a.ends_with(['x', 'X']) { cur.height * v } else if height > 0.0 { v / height } else { cur.height };
                        }
                    }
                    'W' => cur.width = number(&arg()).unwrap_or(cur.width),
                    'T' => cur.tracking = number(&arg()).unwrap_or(cur.tracking),
                    'Q' => cur.oblique = arg().trim().parse().unwrap_or(cur.oblique),
                    'C' => cur.color = arg().trim().parse::<i64>().ok().filter(|i| (1..=255).contains(i)).map(super::aci),
                    // \c is 0xBBGGRR.
                    'c' => cur.color = arg().trim().parse::<u32>().ok().map(|v| (v & 0xFF) << 16 | (v & 0xFF00) | (v >> 16 & 0xFF)),
                    'f' | 'F' => cur.font = font(&arg()),
                    'S' => {
                        let a = arg();
                        // The first unescaped separator splits top from bottom.
                        let (mut top, mut bottom, mut kind, mut escaped) = (String::new(), String::new(), None, false);
                        for ch in a.chars() {
                            if ch == '\\' && !escaped {
                                escaped = true;
                                continue;
                            }
                            if matches!(ch, '/' | '#' | '^') && !escaped && kind.is_none() {
                                kind = Some(ch);
                            } else if kind.is_none() {
                                top.push(ch);
                            } else {
                                bottom.push(ch);
                            }
                            escaped = false;
                        }
                        let stack = format!("{STACK}{top}{STACK_OVER}{bottom}{STACK_KIND}{}{STACK_END}", kind.unwrap_or('/'));
                        put(&mut out, &cur, &stack);
                    }
                    // Other codes with an argument (alignment, paragraph, …) do not change the glyphs.
                    'A' | 'p' => drop(arg()),
                    other => put(&mut out, &cur, other.encode_utf8(&mut [0; 4])),
                }
            }
            c => put(&mut out, &cur, c.encode_utf8(&mut [0; 4])),
        }
    }
    if out.contains([STYLE, STACK]) { out } else { mtext_plain(s) }
}

/// Font family and style bits (`dxf::FONT_*`) of an MTEXT `\f` argument such as `Arial|b1|i0|c0|p34`.
fn font(arg: &str) -> u8 {
    let mut parts = arg.split('|');
    let name = parts.next().unwrap_or("").to_lowercase();
    let family = if ["courier", "mono", "consol", "lucida console"].iter().any(|n| name.contains(n)) {
        super::FONT_MONO
    } else if ["times", "roman", "georgia", "garamond", "serif"].iter().any(|n| name.contains(n)) && !name.contains("sans") {
        super::FONT_SERIF
    } else {
        super::FONT_SANS
    };
    parts.fold(family, |f, p| match p {
        "b1" => f | super::FONT_BOLD,
        "i1" => f | super::FONT_ITALIC,
        _ => f,
    })
}

/// TEXT with its `%%u`, `%%o` and `%%k` toggles as rich-text underline, overline and strike-through,
/// or plain text (as `text_codes`) if it has none.
pub fn text_rich(s: &str) -> String {
    if !["%%u", "%%o", "%%k"].iter().any(|code| s.to_lowercase().contains(code)) {
        return text_codes(s);
    }
    // As MTEXT: literal backslashes and braces escaped, each toggle a switch.
    let mut mtext = String::with_capacity(s.len() + 8);
    let mut on = [false; 3];
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let toggle = ['u', 'o', 'k'].iter().position(|&k| rest.starts_with("%%") && rest[2..].starts_with([k, k.to_ascii_uppercase()]));
        if let Some(i) = toggle {
            on[i] = !on[i];
            mtext.push('\\');
            mtext.push(if on[i] { ['L', 'O', 'K'][i] } else { ['l', 'o', 'k'][i] });
            rest = &rest[3..];
            continue;
        }
        if matches!(c, '\\' | '{' | '}') {
            mtext.push('\\');
        }
        mtext.push(c);
        rest = &rest[c.len_utf8()..];
    }
    mtext_rich(&mtext, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_mtext_marks_styles_and_stacks() {
        assert_eq!(mtext_rich("plain\\Ptext", 2.0), "plain\ntext");
        let r = mtext_rich("a{\\H2x;b}c \\S1/2;", 2.0);
        assert_eq!(r, format!("a{STYLE}2;1;1;0;-;0;0{STYLE_END}b{STYLE}1;1;1;0;-;0;0{STYLE_END}c {STACK}1{STACK_OVER}2{STACK_KIND}/{STACK_END}"));
        assert!(mtext_rich("{\\H5;x}", 2.5).starts_with(&format!("{STYLE}2;")));
        // \c is BGR: 16744448 = 0xFF8000 is blue (0, 128, 255).
        assert!(mtext_rich("{\\c16744448;x}", 2.5).starts_with(&format!("{STYLE}1;1;1;0;0080ff;")));
    }

    #[test]
    fn text_toggles_become_rich_lines() {
        assert_eq!(text_rich("%%c1"), "Ø1");
        let r = text_rich("a %%uunder%%u b\\c");
        assert_eq!(r, format!("a {STYLE}1;1;1;0;-;0;1{STYLE_END}under{STYLE}1;1;1;0;-;0;0{STYLE_END} b\\c"));
    }
}
