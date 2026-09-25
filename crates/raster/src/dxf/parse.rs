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
