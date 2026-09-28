//! Minimal ZIP reader for DWF 6 and DWFx packages: stored and deflated entries, no ZIP64.

use std::io::Read;

const EOCD: u32 = 0x0605_4B50;
const CENTRAL: u32 = 0x0201_4B50;
const LOCAL: u32 = 0x0403_4B50;
/// Largest decompressed entry, against zip bombs.
const MAX_ENTRY: u64 = 1 << 30;

struct Entry {
    name: String,
    method: u16,
    flags: u16,
    compressed: usize,
    size: u64,
    /// Offset of the local header, already corrected for data before the archive.
    local: usize,
}

pub(super) struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Part names compare case-insensitively, with `/` separators and percent-escapes decoded (OPC).
pub(super) fn normalize(name: &str) -> String {
    let name = name.replace('\\', "/");
    let bytes = name.trim_start_matches('/').as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_lowercase()
}

impl<'a> Zip<'a> {
    /// Reads the central directory. Bytes before the archive (the `(DWF V06.00)` header) are allowed.
    pub(super) fn open(data: &'a [u8]) -> Result<Zip<'a>, String> {
        let corrupt = || "Corrupt ZIP package".to_string();
        let search = data.len().saturating_sub(22 + 0xFFFF);
        let eocd = (search..data.len().saturating_sub(21)).rev().find(|&i| u32_at(data, i) == Some(EOCD)).ok_or("Not a ZIP package")?;
        let count = u16_at(data, eocd + 10).ok_or_else(corrupt)? as usize;
        let cd_size = u32_at(data, eocd + 12).ok_or_else(corrupt)? as usize;
        let cd_offset = u32_at(data, eocd + 16).ok_or_else(corrupt)? as usize;
        let prefix = eocd.checked_sub(cd_offset + cd_size).ok_or_else(corrupt)?;
        let mut entries = Vec::with_capacity(count);
        let mut at = prefix + cd_offset;
        for _ in 0..count {
            if u32_at(data, at) != Some(CENTRAL) {
                return Err(corrupt());
            }
            let field = |off| u16_at(data, at + off).ok_or_else(corrupt);
            let (flags, method, name_len, extra, comment) = (field(8)?, field(10)?, field(28)? as usize, field(30)? as usize, field(32)? as usize);
            let compressed = u32_at(data, at + 20).ok_or_else(corrupt)? as usize;
            let size = u32_at(data, at + 24).ok_or_else(corrupt)? as u64;
            let local = prefix + u32_at(data, at + 42).ok_or_else(corrupt)? as usize;
            let name = data.get(at + 46..at + 46 + name_len).ok_or_else(corrupt)?;
            entries.push(Entry { name: String::from_utf8_lossy(name).into_owned(), method, flags, compressed, size, local });
            at += 46 + name_len + extra + comment;
        }
        Ok(Zip { data, entries })
    }

    /// Entry names in archive order.
    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// The entry named `name` (see [`normalize`]), or `None` if there is none.
    pub(super) fn read(&self, name: &str) -> Option<Result<Vec<u8>, String>> {
        let key = normalize(name);
        let e = self.entries.iter().find(|e| normalize(&e.name) == key)?;
        Some(self.extract(e))
    }

    fn extract(&self, e: &Entry) -> Result<Vec<u8>, String> {
        let corrupt = || format!("Corrupt ZIP entry {}", e.name);
        if e.flags & 1 != 0 {
            return Err(format!("{} is encrypted", e.name));
        }
        if e.size > MAX_ENTRY {
            return Err(format!("{} is too large", e.name));
        }
        if u32_at(self.data, e.local) != Some(LOCAL) {
            return Err(corrupt());
        }
        let name_len = u16_at(self.data, e.local + 26).ok_or_else(corrupt)? as usize;
        let extra = u16_at(self.data, e.local + 28).ok_or_else(corrupt)? as usize;
        let start = e.local + 30 + name_len + extra;
        let raw = self.data.get(start..start + e.compressed).ok_or_else(corrupt)?;
        match e.method {
            0 => Ok(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(e.size as usize);
                flate2::read::DeflateDecoder::new(raw).take(MAX_ENTRY).read_to_end(&mut out).map_err(|_| corrupt())?;
                Ok(out)
            }
            m => Err(format!("{}: unsupported ZIP compression method {m}", e.name)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A ZIP archive of `(name, data)` entries, deflated if `deflate`.
    pub(crate) fn build(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in entries {
            let body = if deflate {
                let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                z.write_all(data).unwrap();
                z.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            let common = |v: &mut Vec<u8>| {
                v.extend(20u16.to_le_bytes()); // version needed
                v.extend(0u16.to_le_bytes()); // flags
                v.extend(method.to_le_bytes());
                v.extend([0; 8]); // time, date, crc (not checked)
                v.extend((body.len() as u32).to_le_bytes());
                v.extend((data.len() as u32).to_le_bytes());
                v.extend((name.len() as u16).to_le_bytes());
                v.extend(0u16.to_le_bytes()); // extra
            };
            out.extend(LOCAL.to_le_bytes());
            common(&mut out);
            out.extend(name.as_bytes());
            out.extend(&body);
            central.extend(CENTRAL.to_le_bytes());
            central.extend(20u16.to_le_bytes()); // version made by
            common(&mut central);
            central.extend([0; 10]); // comment length, disk, attributes
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend(&central);
        out.extend(EOCD.to_le_bytes());
        out.extend([0; 4]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    #[test]
    fn stored_and_deflated_with_prefix() {
        for deflate in [false, true] {
            let zip = [b"(DWF V06.00)".as_slice(), &build(&[("a.txt", b"hello"), ("Dir\\B%20c.xml", b"<x/>")], deflate)].concat();
            let z = Zip::open(&zip).unwrap();
            assert_eq!(z.names().collect::<Vec<_>>(), ["a.txt", "Dir\\B%20c.xml"]);
            assert_eq!(z.read("A.TXT").unwrap().unwrap(), b"hello");
            assert_eq!(z.read("/dir/b c.xml").unwrap().unwrap(), b"<x/>");
            assert!(z.read("missing").is_none());
        }
    }

    #[test]
    fn rejects_non_zip() {
        assert!(Zip::open(b"(DWF V00.55)L 0,0 1,1").is_err());
    }
}
