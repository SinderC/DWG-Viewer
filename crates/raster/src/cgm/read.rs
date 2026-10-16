//! The binary encoding (ISO 8632-3): the command stream and parameter reading at the current precisions.

use std::borrow::Cow;

use crate::dxf::P;

/// One element: class, id and its parameter bytes (partitions joined, padding removed).
pub(super) struct Command<'a> {
    pub class: u8,
    pub id: u8,
    pub data: Cow<'a, [u8]>,
}

/// Iterates the commands of a binary metafile.
pub(super) struct Commands<'a> {
    data: &'a [u8],
    pos: usize,
}

pub(super) fn commands(data: &[u8]) -> Commands<'_> {
    Commands { data, pos: 0 }
}

impl Commands<'_> {
    fn word(&mut self) -> Option<u16> {
        let w = self.data.get(self.pos..self.pos + 2)?;
        self.pos += 2;
        Some(u16::from_be_bytes([w[0], w[1]]))
    }

    /// `len` bytes, then the padding byte that keeps the next word aligned.
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let bytes = self.data.get(self.pos..self.pos + len)?;
        self.pos += len + (len & 1);
        Some(bytes)
    }
}

impl<'a> Iterator for Commands<'a> {
    type Item = Result<Command<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        let truncated = || Some(Err("The file ends in the middle of an element".to_string()));
        let head = self.word()?;
        let (class, id, len) = ((head >> 12) as u8, (head >> 5 & 0x7F) as u8, (head & 0x1F) as usize);
        if len < 31 {
            let data = self.data;
            let start = self.pos;
            return match self.take(len) {
                Some(_) => Some(Ok(Command { class, id, data: Cow::Borrowed(&data[start..start + len]) })),
                None => truncated(),
            };
        }
        // Long form: partitions of up to 32767 bytes, each flagged if another follows.
        let mut joined = Vec::new();
        loop {
            let Some(w) = self.word() else { return truncated() };
            let Some(part) = self.take((w & 0x7FFF) as usize) else { return truncated() };
            joined.extend_from_slice(part);
            if w & 0x8000 == 0 {
                return Some(Ok(Command { class, id, data: Cow::Owned(joined) }));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Real {
    Fixed32,
    Fixed64,
    Float32,
    Float64,
}

impl Real {
    /// From REAL PRECISION's (form, exponent or whole bits, fraction bits).
    pub fn new(form: i64, a: i64, b: i64) -> Result<Real, String> {
        match (form, a + b) {
            (0, 32) => Ok(Real::Float32),
            (0, 64) => Ok(Real::Float64),
            (1, 32) => Ok(Real::Fixed32),
            (1, 64) => Ok(Real::Fixed64),
            _ => Err(format!("Unsupported real precision ({form}, {a}, {b})")),
        }
    }
}

/// Field sizes in bytes (colours in bits, as cell arrays may pack them), as the descriptor and
/// control elements set them.
#[derive(Clone, Debug)]
pub(super) struct Precision {
    pub int: usize,
    pub index: usize,
    pub real: Real,
    pub color_bits: usize,
    pub color_index_bits: usize,
    /// Colour components: 3 (RGB) or 4 (CMYK).
    pub components: usize,
    pub vdc_real: bool,
    pub vdc_int: usize,
    pub vdc_float: Real,
}

impl Default for Precision {
    fn default() -> Self {
        Precision { int: 2, index: 2, real: Real::Fixed32, color_bits: 8, color_index_bits: 8, components: 3, vdc_real: false, vdc_int: 2, vdc_float: Real::Fixed32 }
    }
}

/// Integer precision in bits (8, 16, 24 or 32) to bytes.
pub(super) fn bytes(bits: i64) -> Result<usize, String> {
    match bits {
        8 | 16 | 24 | 32 => Ok(bits as usize / 8),
        _ => Err(format!("Unsupported precision of {bits} bits")),
    }
}

/// Reads the parameters of one command.
pub(super) struct Params<'a> {
    data: &'a [u8],
    pos: usize,
    pub p: &'a Precision,
}

pub(super) fn params<'a>(data: &'a [u8], p: &'a Precision) -> Params<'a> {
    Params { data, pos: 0, p }
}

pub(super) type R<T> = Result<T, String>;

fn short() -> String {
    "element parameters are truncated".into()
}

impl<'a> Params<'a> {
    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn bytes(&mut self, n: usize) -> R<&'a [u8]> {
        let b = self.data.get(self.pos..self.pos + n).ok_or_else(short)?;
        self.pos += n;
        Ok(b)
    }

    /// The rest of the parameters (a bitstream).
    pub fn rest(&mut self) -> &'a [u8] {
        let b = self.data.get(self.pos..).unwrap_or_default();
        self.pos = self.data.len();
        b
    }

    fn signed(&mut self, n: usize) -> R<i64> {
        let v = self.unsigned(n)?;
        let shift = 64 - 8 * n as u32;
        Ok(((v as i64) << shift) >> shift)
    }

    fn unsigned(&mut self, n: usize) -> R<u64> {
        Ok(self.bytes(n)?.iter().fold(0, |v, &b| v << 8 | b as u64))
    }

    pub fn int(&mut self) -> R<i64> {
        self.signed(self.p.int)
    }

    pub fn index(&mut self) -> R<i64> {
        self.signed(self.p.index)
    }

    /// Enumerated values are always 16 bits.
    pub fn enumeration(&mut self) -> R<i64> {
        self.signed(2)
    }

    pub fn real(&mut self) -> R<f64> {
        self.real_as(self.p.real)
    }

    pub fn real_as(&mut self, r: Real) -> R<f64> {
        Ok(match r {
            Real::Fixed32 => self.signed(2)? as f64 + self.unsigned(2)? as f64 / 65536.0,
            Real::Fixed64 => self.signed(4)? as f64 + self.unsigned(4)? as f64 / 4294967296.0,
            Real::Float32 => f32::from_bits(self.unsigned(4)? as u32) as f64,
            Real::Float64 => f64::from_bits(self.unsigned(8)?),
        })
    }

    pub fn vdc(&mut self) -> R<f64> {
        if self.p.vdc_real { self.real_as(self.p.vdc_float) } else { Ok(self.signed(self.p.vdc_int)? as f64) }
    }

    pub fn point(&mut self) -> R<P> {
        Ok([self.vdc()?, self.vdc()?])
    }

    /// Points up to the end of the parameters.
    pub fn points(&mut self) -> R<Vec<P>> {
        let mut out = Vec::new();
        while !self.is_empty() {
            out.push(self.point()?);
        }
        Ok(out)
    }

    pub fn color_index(&mut self) -> R<u32> {
        Ok(self.unsigned(self.p.color_index_bits / 8)? as u32)
    }

    /// Direct colour components at the colour precision.
    pub fn color_direct(&mut self) -> R<[u32; 4]> {
        let mut c = [0; 4];
        for v in c.iter_mut().take(self.p.components) {
            *v = self.unsigned(self.p.color_bits / 8)? as u32;
        }
        Ok(c)
    }

    /// A string: a length byte, or 255 and 16-bit lengths whose top bit flags another chunk.
    /// Characters are taken as Latin-1.
    pub fn string(&mut self) -> R<String> {
        Ok(self.string_bytes()?.into_iter().map(char::from).collect())
    }

    pub fn string_bytes(&mut self) -> R<Vec<u8>> {
        let n = self.bytes(1)?[0];
        if n < 255 {
            return Ok(self.bytes(n as usize)?.to_vec());
        }
        let mut out = Vec::new();
        loop {
            let w = self.unsigned(2)?;
            out.extend_from_slice(self.bytes((w & 0x7FFF) as usize)?);
            if w & 0x8000 == 0 {
                return Ok(out);
            }
        }
    }

    /// The first string member of a structured data record, if it starts with one.
    pub fn sdr_string(&mut self) -> R<Option<String>> {
        let record = self.string_bytes()?;
        let mut r = Params { data: &record, pos: 0, p: self.p };
        let kind = r.index()?;
        let count = r.int()?;
        // Member types 13 (S) and 14 (SF).
        Ok(if matches!(kind, 13 | 14) && count > 0 { Some(r.string()?) } else { None })
    }
}

/// Reads packed values of `bits` bits, most significant first.
pub(super) struct Bits<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, bit: 0 }
    }

    pub fn read(&mut self, bits: usize) -> R<u32> {
        let mut v = 0u64;
        for _ in 0..bits {
            let byte = *self.data.get(self.bit / 8).ok_or_else(short)?;
            v = v << 1 | (byte >> (7 - self.bit % 8) & 1) as u64;
            self.bit += 1;
        }
        Ok(v as u32)
    }

    /// Skips to the next 16-bit boundary, where each cell array row starts.
    pub fn align(&mut self) {
        self.bit = self.bit.next_multiple_of(16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_long_and_partitioned_commands() {
        // POLYLINE with 3 bytes (padded), then a long-form element in two partitions.
        let mut data = vec![0x40, 0x23, 1, 2, 3, 0];
        data.extend([0x40, 0x3F, 0x80, 0x01, 9, 0, 0x00, 0x02, 7, 8]);
        let cmds: Vec<_> = commands(&data).map(Result::unwrap).collect();
        assert_eq!((cmds[0].class, cmds[0].id, &cmds[0].data[..]), (4, 1, &[1, 2, 3][..]));
        assert_eq!(&cmds[1].data[..], &[9, 7, 8]);
        assert!(commands(&[0x40, 0x24, 1]).next().unwrap().is_err());
    }

    #[test]
    fn numbers_at_each_precision() {
        let p = Precision { int: 3, ..Default::default() };
        let mut r = params(&[0xFF, 0xFF, 0xFE, 0xFF, 0xFF, 0x80, 0x00], &p);
        assert_eq!(r.int().unwrap(), -2);
        // Fixed 16.16: -1 + 0.5.
        assert_eq!(r.real_as(Real::Fixed32).unwrap(), -0.5);
        let mut r = params(&[0x3F, 0xC0, 0, 0], &p);
        assert_eq!(r.real_as(Real::Float32).unwrap(), 1.5);
        let p = Precision { vdc_real: true, vdc_float: Real::Float64, ..Default::default() };
        let mut r = params(&[0x40, 0x04, 0, 0, 0, 0, 0, 0], &p);
        assert_eq!(r.vdc().unwrap(), 2.5);
        assert!(r.vdc().is_err());
    }

    #[test]
    fn strings_and_bits() {
        let p = Precision::default();
        let mut r = params(b"\x02hi\xFF\x80\x01a\x00\x02bc", &p);
        assert_eq!(r.string().unwrap(), "hi");
        assert_eq!(r.string().unwrap(), "abc");
        let mut b = Bits::new(&[0b1011_0000, 0, 0xF0]);
        assert_eq!(b.read(2).unwrap(), 2);
        assert_eq!(b.read(2).unwrap(), 3);
        b.align();
        assert_eq!(b.read(4).unwrap(), 15);
    }
}
