//! HP-GL tokenizer: two-letter mnemonics with numeric parameters, label text and PE data.

const ETX: u8 = 0x03;
const ESC: u8 = 0x1B;

pub struct Command {
    /// Upper-case mnemonic.
    pub op: [u8; 2],
    pub args: Vec<f64>,
    /// Label text (LB), encoded data (PE) or symbol character (SM).
    pub text: Vec<u8>,
}

/// Splits HP-GL into commands. Keeps the label terminator (DT) across calls, so a file split
/// into several HP-GL/2 blocks by PCL escapes is read as one stream.
pub struct Lexer {
    terminator: u8,
}

impl Default for Lexer {
    fn default() -> Self {
        Lexer { terminator: ETX }
    }
}

impl Lexer {
    pub fn run(&mut self, data: &[u8], mut f: impl FnMut(Command)) {
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            if c == ESC && data.get(i + 1) == Some(&b'.') {
                i = skip_device_control(data, i);
                continue;
            }
            if !c.is_ascii_alphabetic() || !data.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
                i += 1;
                continue;
            }
            let op = [c.to_ascii_uppercase(), data[i + 1].to_ascii_uppercase()];
            i += 2;
            let mut text = Vec::new();
            match &op {
                b"LB" | b"WD" => {
                    let end = data[i..].iter().position(|&b| b == self.terminator).map_or(data.len(), |n| i + n);
                    text = data[i..end].to_vec();
                    i = end + 1;
                }
                b"DT" => {
                    // DT t[,mode]; — a bare `DT;` restores ETX.
                    match data.get(i) {
                        None | Some(b';') => self.terminator = ETX,
                        Some(&t) => {
                            self.terminator = t;
                            i += 1;
                        }
                    }
                    let (_, next) = numbers(data, i);
                    i = next;
                }
                b"PE" => {
                    let end = data[i..].iter().position(|&b| b == b';').map_or(data.len(), |n| i + n);
                    text = data[i..end].to_vec();
                    i = end + 1;
                }
                b"SM" => {
                    if let Some(&c) = data.get(i).filter(|&&c| c != b';' && c > b' ') {
                        text.push(c);
                        i += 1;
                    }
                }
                _ => {}
            }
            let (args, next) = if matches!(&op, b"LB" | b"WD" | b"PE" | b"DT") { (Vec::new(), i) } else { numbers(data, i) };
            i = next;
            f(Command { op, args, text });
        }
    }
}

/// Numeric parameters from `i`, separated by commas, blanks or signs, up to a `;` or the next
/// mnemonic. Quoted strings (BP, CO) are skipped. Returns the numbers and where parsing stopped.
fn numbers(data: &[u8], mut i: usize) -> (Vec<f64>, usize) {
    let mut out = Vec::new();
    while i < data.len() {
        match data[i] {
            b' ' | b',' | b'\t' | b'\r' | b'\n' => i += 1,
            b';' => return (out, i + 1),
            b'"' => i = data[i + 1..].iter().position(|&b| b == b'"').map_or(data.len(), |n| i + n + 2),
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < data.len() && matches!(data[i], b'.' | b'0'..=b'9') {
                    i += 1;
                }
                if let Some(v) = std::str::from_utf8(&data[start..i]).ok().and_then(|s| s.parse().ok()) {
                    out.push(v);
                }
            }
            _ => return (out, i),
        }
    }
    (out, i)
}

/// Skips an HP-GL/1 device-control sequence (`ESC . letter [params] [:]`) starting at `i`.
fn skip_device_control(data: &[u8], i: usize) -> usize {
    let mut j = (i + 3).min(data.len());
    let params = data[j..].iter().take_while(|&&b| matches!(b, b'0'..=b'9' | b';' | b'+' | b'-' | b'.' | b' ')).count();
    if data.get(j + params) == Some(&b':') {
        j += params + 1;
    }
    j
}

/// One item of Polyline Encoded (PE) data.
#[derive(Debug, PartialEq)]
pub enum Pe {
    Pen(i64),
    /// Next coordinate pair is a pen-up move.
    Up,
    /// Next coordinate pair is absolute.
    Absolute,
    /// Coordinate pair, in user units.
    Point(f64, f64),
}

/// Decodes PE data: flags `:` pen, `<` pen up, `>` fractional bits, `=` absolute, `7` 7-bit mode;
/// numbers in base 64 (8-bit) or base 32 (7-bit), sign in the lowest bit.
pub fn decode_pe(data: &[u8]) -> Vec<Pe> {
    let mut out = Vec::new();
    let (mut base32, mut frac) = (false, 0.0);
    let mut i = 0;
    let number = |i: &mut usize, base32: bool| -> Option<f64> {
        let (mut value, mut mult) = (0u64, 1u64);
        while let Some(&c) = data.get(*i) {
            *i += 1;
            let (digit, last) = match (c, base32) {
                (63..=94, true) => (c - 63, false),
                (95..=126, true) => (c - 95, true),
                (63..=126, false) => (c - 63, false),
                (191..=254, false) => (c - 191, true),
                _ => continue,
            };
            value = value.saturating_add((digit as u64).saturating_mul(mult));
            mult = mult.saturating_mul(if base32 { 32 } else { 64 });
            if last {
                let magnitude = (value >> 1) as f64;
                return Some(if value & 1 == 1 { -magnitude } else { magnitude });
            }
        }
        None
    };
    while i < data.len() {
        let c = data[i];
        match c {
            b':' | b'>' => {
                i += 1;
                let Some(v) = number(&mut i, base32) else { break };
                if c == b':' {
                    out.push(Pe::Pen(v as i64));
                } else {
                    frac = v;
                }
            }
            b'<' => {
                out.push(Pe::Up);
                i += 1;
            }
            b'=' => {
                out.push(Pe::Absolute);
                i += 1;
            }
            b'7' => {
                base32 = true;
                i += 1;
            }
            _ => {
                let Some(x) = number(&mut i, base32) else { break };
                let Some(y) = number(&mut i, base32) else { break };
                let scale = 2f64.powf(-frac);
                out.push(Pe::Point(x * scale, y * scale));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &[u8]) -> Vec<(String, Vec<f64>, Vec<u8>)> {
        let mut out = Vec::new();
        Lexer::default().run(s, |c| out.push((String::from_utf8_lossy(&c.op).into_owned(), c.args, c.text)));
        out
    }

    #[test]
    fn commands() {
        let cmds = lex(b"IN;SP1;pa10,20PD30 -40,-5.5\n50;LBHello\x03PU;");
        let ops: Vec<_> = cmds.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(ops, ["IN", "SP", "PA", "PD", "LB", "PU"]);
        assert_eq!(cmds[2].1, [10.0, 20.0]);
        assert_eq!(cmds[3].1, [30.0, -40.0, -5.5, 50.0]);
        assert_eq!(cmds[4].2, b"Hello");
    }

    #[test]
    fn terminator_and_device_control() {
        let cmds = lex(b"\x1b.(\x1b.I81;;17:DT*,1;LBA;B*SI.2,.3;BP\"x;y\",1;");
        let ops: Vec<_> = cmds.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(ops, ["DT", "LB", "SI", "BP"]);
        assert_eq!(cmds[1].2, b"A;B");
        assert_eq!(cmds[2].1, [0.2, 0.3]);
        assert_eq!(cmds[3].1, [1.0]);
    }

    /// Encodes `v` like a PE number (base 64, 8-bit).
    fn enc(v: i64) -> Vec<u8> {
        let mut n = (v.unsigned_abs() << 1) | (v < 0) as u64;
        let mut out = Vec::new();
        loop {
            let digit = (n % 64) as u8;
            n /= 64;
            if n == 0 {
                out.push(191 + digit);
                return out;
            }
            out.push(63 + digit);
        }
    }

    #[test]
    fn polyline_encoded() {
        let mut data = vec![b':'];
        data.extend(enc(2));
        data.push(b'<');
        data.push(b'=');
        data.extend(enc(1000));
        data.extend(enc(-1000));
        data.extend(enc(5));
        data.extend(enc(-7));
        assert_eq!(decode_pe(&data), [Pe::Pen(2), Pe::Up, Pe::Absolute, Pe::Point(1000.0, -1000.0), Pe::Point(5.0, -7.0)]);
        // 7-bit: 3 → 6 → terminating digit 95 + 6; with 1 fractional bit.
        assert_eq!(decode_pe(&[b'7', b'>', 95 + 2, 95 + 6, 95 + 7]), [Pe::Point(1.5, -1.5)]);
    }
}
