//! CALS raster (MIL-R-28002) Type 1: 2048-byte ASCII header + CCITT G4 data.

use crate::bitmap::Bitmap;

pub const HEADER_LEN: usize = 2048;
const RECORD_LEN: usize = 128;

#[derive(Debug, PartialEq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    /// Pel path direction and line progression, in degrees (e.g. 0, 270).
    pub orient: (u32, u32),
}

pub fn parse_header(data: &[u8]) -> Result<Header, String> {
    if data.len() < HEADER_LEN {
        return Err("File is too small to be a CALS raster file".into());
    }
    let mut rtype = None;
    let mut size = None;
    let mut dpi = None;
    let mut orient = (0, 270);

    for record in data[..HEADER_LEN].chunks(RECORD_LEN) {
        let text = String::from_utf8_lossy(record);
        let Some((key, value)) = text.split_once(':') else { continue };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "rtype" => rtype = Some(value.to_string()),
            "rpelcnt" => size = Some(parse_pair(value, "rpelcnt")?),
            "rdensty" => dpi = value.parse::<u32>().ok(),
            "rorient" => orient = parse_pair(value, "rorient")?,
            _ => {}
        }
    }

    match rtype.as_deref() {
        Some("1") => {}
        Some(t) => return Err(format!("Unsupported CALS raster type {t} (only type 1 is supported)")),
        None => return Err("Not a CALS raster file (missing rtype)".into()),
    }
    let (width, height) = size.ok_or("Missing rpelcnt in CALS header")?;
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(format!("Unsupported image size {width}×{height}"));
    }
    if orient.0 % 90 != 0 || orient.1 % 90 != 0 || orient.1 % 180 == 0 {
        return Err(format!("Invalid rorient {:03},{:03}", orient.0, orient.1));
    }
    Ok(Header { width, height, dpi: dpi.unwrap_or(0), orient })
}

fn parse_pair(value: &str, key: &str) -> Result<(u32, u32), String> {
    let err = || format!("Invalid {key} value '{value}'");
    let (a, b) = value.split_once(',').ok_or_else(err)?;
    Ok((a.trim().parse().map_err(|_| err())?, b.trim().parse().map_err(|_| err())?))
}

/// The header records that have a value, as (key, value) rows for display.
pub fn info(data: &[u8]) -> Vec<(String, String)> {
    let mut rows = vec![("Compression".to_string(), "CCITT Group 4".to_string())];
    for record in data[..HEADER_LEN.min(data.len())].chunks(RECORD_LEN) {
        let text = String::from_utf8_lossy(record);
        if let Some((key, value)) = text.split_once(':') {
            let value = value.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            if !value.is_empty() {
                rows.push((key.trim().to_string(), value.to_string()));
            }
        }
    }
    rows
}

/// Decodes a CALS file into an upright bitmap.
pub fn decode(data: &[u8]) -> Result<(Header, Bitmap), String> {
    let header = parse_header(data)?;
    let mut bitmap = Bitmap::new(header.width, header.height);
    let mut y = 0;
    fax::decoder::decode_g4(
        data[HEADER_LEN..].iter().copied(),
        header.width as u16,
        Some(header.height as u16),
        |transitions| {
            bitmap.set_row_from_transitions(y, transitions);
            y += 1;
        },
    )
    .ok_or("Corrupt or unsupported CCITT G4 image data")?;
    let bitmap = bitmap.oriented(header.orient.0, header.orient.1);
    Ok((header, bitmap))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn make_header(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in fields {
            let mut rec = format!("{k}: {v}").into_bytes();
            rec.resize(RECORD_LEN, b' ');
            out.extend(rec);
        }
        out.resize(HEADER_LEN, b' ');
        out
    }

    #[test]
    fn parses_header() {
        let h = make_header(&[("rtype", "1"), ("rorient", "000,270"), ("rpelcnt", "002200,001700"), ("rdensty", "0200")]);
        assert_eq!(parse_header(&h).unwrap(), Header { width: 2200, height: 1700, dpi: 200, orient: (0, 270) });
    }

    #[test]
    fn info_lists_header_records() {
        let h = make_header(&[("srcdocid", "DWG-42"), ("notes", ""), ("rtype", "1")]);
        let rows = info(&h);
        assert_eq!(rows[1..], [("srcdocid".to_string(), "DWG-42".to_string()), ("rtype".to_string(), "1".to_string())]);
    }

    #[test]
    fn rejects_type_2() {
        let h = make_header(&[("rtype", "2"), ("rpelcnt", "10,10")]);
        assert!(parse_header(&h).unwrap_err().contains("type 2"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_header(&[0u8; 100]).is_err());
        assert!(parse_header(&[b'x'; HEADER_LEN]).is_err());
        let h = make_header(&[("rtype", "1"), ("rpelcnt", "abc")]);
        assert!(parse_header(&h).is_err());
    }

    #[test]
    fn g4_roundtrip() {
        use fax::{encoder::Encoder, Color, VecWriter};
        let (w, h) = (37u32, 23u32);
        let black = |x: u32, y: u32| (x * 7 + y * 3) % 11 < 4 || x == y;
        let mut enc = Encoder::new(VecWriter::new());
        for y in 0..h {
            let pels = (0..w).map(|x| if black(x, y) { Color::Black } else { Color::White });
            enc.encode_line(pels, w as u16).unwrap();
        }
        let mut data = make_header(&[("rtype", "1"), ("rpelcnt", &format!("{w},{h}")), ("rdensty", "300")]);
        data.extend(enc.finish().unwrap().finish());

        let (header, bmp) = decode(&data).unwrap();
        assert_eq!((header.width, header.height, header.dpi), (w, h, 300));
        for y in 0..h {
            for x in 0..w {
                assert_eq!(bmp.get(x, y), black(x, y), "pixel {x},{y}");
            }
        }
    }
}
