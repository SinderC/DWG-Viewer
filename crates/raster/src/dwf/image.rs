//! Raster images embedded in W2D streams, decoded to the viewer's image types.
//!
//! Rows are taken top first. Transparency is flattened onto white, which the vector renderer treats
//! as see-through paper.

use std::io::Read;

use crate::bitmap::{Bitmap, MAX_BILEVEL_PIXELS};
use crate::rgba::{rgb, RgbaImage, MAX_PIXELS};
use crate::tiff::Image;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    /// One byte per pixel into the stream's colour map.
    Indexed,
    /// One byte per pixel into the image's own colour map.
    Mapped,
    Rgb,
    Rgba,
    Jpeg,
    Png,
    /// CCITT Group 4, 1 = black.
    Group4,
    /// CCITT Group 4 with a two-colour map.
    Group4Mapped,
}

/// Decodes `data`; `map` holds 0xRRGGBB colours for the mapped kinds.
pub(super) fn decode(kind: Kind, width: u32, height: u32, map: &[u32], data: &[u8]) -> Result<Image, String> {
    let short = || "image data is truncated".to_string();
    let lookup = |i: u8| map.get(i as usize).copied().unwrap_or(0);
    let raw = |bytes_per_pixel: usize| -> Result<&[u8], String> {
        let n = pixel_count(width, height, MAX_PIXELS)?;
        data.get(..n * bytes_per_pixel).ok_or_else(short)
    };
    match kind {
        Kind::Indexed | Kind::Mapped => Ok(color(width, height, raw(1)?.iter().map(|&i| lookup(i)))),
        Kind::Rgb => Ok(color(width, height, raw(3)?.chunks_exact(3).map(|p| packed(p[0], p[1], p[2])))),
        Kind::Rgba => Ok(color(width, height, raw(4)?.chunks_exact(4).map(|p| over_white(p[0], p[1], p[2], p[3])))),
        Kind::Jpeg => jpeg(data),
        Kind::Png => png(data),
        Kind::Group4 => group4(width, height, data).map(Image::Bilevel),
        Kind::Group4Mapped => {
            let bitmap = group4(width, height, data)?;
            let (paper, ink) = (lookup(0), lookup(1));
            if (paper, ink) == (0xFFFFFF, 0) {
                return Ok(Image::Bilevel(bitmap));
            }
            pixel_count(width, height, MAX_PIXELS)?;
            let px = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).map(|(x, y)| if bitmap.get(x, y) { ink } else { paper });
            Ok(color(width, height, px))
        }
    }
}

fn pixel_count(width: u32, height: u32, limit: u64) -> Result<usize, String> {
    let n = width as u64 * height as u64;
    if n == 0 || n > limit {
        return Err(format!("{width} × {height} image is empty or too large"));
    }
    Ok(n as usize)
}

fn packed(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

fn over_white(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let mix = |c: u8| (c as u32 * a as u32 / 255 + 255 - a as u32) as u8;
    packed(mix(r), mix(g), mix(b))
}

/// An RGB image from 0xRRGGBB pixels, row by row.
fn color(width: u32, height: u32, px: impl Iterator<Item = u32>) -> Image {
    let data = px.map(|c| rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)).collect();
    Image::Color(RgbaImage { width, height, data })
}

fn group4(width: u32, height: u32, data: &[u8]) -> Result<Bitmap, String> {
    pixel_count(width, height, MAX_BILEVEL_PIXELS)?;
    let (w, h) = (u16::try_from(width).map_err(|_| "image too wide")?, u16::try_from(height).map_err(|_| "image too tall")?);
    let mut bitmap = Bitmap::new(width, height);
    let mut y = 0;
    fax::decoder::decode_g4(data.iter().copied(), w, Some(h), |transitions| {
        if y < height {
            bitmap.set_row_from_transitions(y, transitions);
        }
        y += 1;
    })
    .ok_or("corrupt Group 4 image data")?;
    Ok(bitmap)
}

fn jpeg(data: &[u8]) -> Result<Image, String> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data.to_vec()), options);
    let err = |e: zune_jpeg::errors::DecodeErrors| format!("JPEG: {e:?}");
    d.decode_headers().map_err(err)?;
    let (w, h) = d.dimensions().ok_or("JPEG without size")?;
    pixel_count(w as u32, h as u32, MAX_PIXELS)?;
    let px = d.decode().map_err(err)?;
    let rgb = px.get(..3 * w * h).ok_or("JPEG data is truncated")?;
    Ok(color(w as u32, h as u32, rgb.chunks_exact(3).map(|p| packed(p[0], p[1], p[2]))))
}

/// Non-interlaced PNG of any colour type and bit depth; 16-bit samples keep their high byte.
fn png(data: &[u8]) -> Result<Image, String> {
    let bad = |what: &str| format!("PNG: {what}");
    let mut rest = data.strip_prefix(b"\x89PNG\r\n\x1a\n").ok_or_else(|| bad("bad signature"))?;
    let (mut header, mut palette, mut idat) = (None, Vec::new(), Vec::new());
    while rest.len() >= 12 {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let body = rest.get(8..8 + len).ok_or_else(|| bad("truncated chunk"))?;
        match &rest[4..8] {
            b"IHDR" if len >= 13 => header = Some(body.to_vec()),
            b"PLTE" => palette = body.chunks_exact(3).map(|p| packed(p[0], p[1], p[2])).collect(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        rest = rest.get(12 + len..).unwrap_or_default();
    }
    let h = header.ok_or_else(|| bad("no header"))?;
    let (w, ht) = (u32::from_be_bytes(h[0..4].try_into().unwrap()), u32::from_be_bytes(h[4..8].try_into().unwrap()));
    let (depth, ctype, interlace) = (h[8] as usize, h[9], h[12]);
    if interlace != 0 {
        return Err(bad("interlaced images are not supported"));
    }
    let channels = match ctype {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        6 => 4,
        _ => return Err(bad("unknown colour type")),
    };
    if !matches!(depth, 1 | 2 | 4 | 8 | 16) {
        return Err(bad("bad bit depth"));
    }
    let n = pixel_count(w, ht, MAX_PIXELS)?;
    let bits = channels * depth;
    let (stride, bpp) = ((w as usize * bits).div_ceil(8), bits.div_ceil(8));
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&idat[..]).take(((stride + 1) * ht as usize) as u64).read_to_end(&mut raw).map_err(|_| bad("corrupt data"))?;
    let rows = unfilter(&raw, stride, bpp, ht as usize).ok_or_else(|| bad("corrupt data"))?;
    // Sample `k` of pixel `x` in `row`, scaled to 8 bits.
    let sample = |row: &[u8], x: usize, k: usize| -> u8 {
        let i = x * channels + k;
        match depth {
            16 => row[2 * i],
            8 => row[i],
            d => {
                let v = row[i * d / 8] >> (8 - d - (i * d) % 8) & ((1 << d) - 1);
                if ctype == 3 { v } else { (v as u32 * 255 / ((1 << d) - 1)) as u8 }
            }
        }
    };
    let mut px = Vec::with_capacity(n);
    for row in rows.chunks_exact(stride) {
        for x in 0..w as usize {
            let s = |k| sample(row, x, k);
            px.push(match ctype {
                0 => packed(s(0), s(0), s(0)),
                3 => palette.get(s(0) as usize).copied().unwrap_or(0),
                4 => over_white(s(0), s(0), s(0), s(1)),
                2 => packed(s(0), s(1), s(2)),
                _ => over_white(s(0), s(1), s(2), s(3)),
            });
        }
    }
    Ok(color(w, ht, px.into_iter()))
}

/// Undoes PNG row filters: each row is a filter byte and `stride` bytes.
fn unfilter(raw: &[u8], stride: usize, bpp: usize, rows: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; stride * rows];
    for y in 0..rows {
        let src = raw.get(y * (stride + 1)..(y + 1) * (stride + 1))?;
        let (done, cur) = out.split_at_mut(y * stride);
        let prev = if y > 0 { &done[(y - 1) * stride..] } else { &[][..] };
        let cur = &mut cur[..stride];
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] as i16 } else { 0 };
            let b = prev.get(i).copied().unwrap_or(0) as i16;
            let c = if i >= bpp { prev.get(i - bpp).copied().unwrap_or(0) as i16 } else { 0 };
            let predict = match src[0] {
                0 => 0,
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let (pa, pb, pc) = ((b - c).abs(), (a - c).abs(), (a + b - 2 * c).abs());
                    if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
                }
                _ => return None,
            };
            cur[i] = src[1 + i].wrapping_add(predict as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn pixels(image: &Image) -> Vec<u32> {
        match image {
            Image::Color(c) => c.data.clone(),
            Image::Bilevel(_) => panic!("expected colour"),
        }
    }

    #[test]
    fn raw_kinds() {
        let map = [0xFF0000, 0x00FF00];
        assert_eq!(pixels(&decode(Kind::Mapped, 2, 1, &map, &[1, 0]).unwrap()), [rgb(0, 255, 0), rgb(255, 0, 0)]);
        assert_eq!(pixels(&decode(Kind::Rgb, 1, 1, &[], &[1, 2, 3]).unwrap()), [rgb(1, 2, 3)]);
        // Fully transparent is white paper.
        assert_eq!(pixels(&decode(Kind::Rgba, 1, 1, &[], &[0, 0, 0, 0]).unwrap()), [rgb(255, 255, 255)]);
        assert!(decode(Kind::Rgb, 2, 2, &[], &[0; 5]).is_err());
    }

    /// A PNG with one IDAT chunk (CRCs are not checked).
    pub(crate) fn png_file(w: u32, h: u32, depth: u8, ctype: u8, extra: &[(&[u8; 4], Vec<u8>)], filtered_rows: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(filtered_rows).unwrap();
        let mut ihdr = [w.to_be_bytes(), h.to_be_bytes()].concat();
        ihdr.extend([depth, ctype, 0, 0, 0]);
        let mut chunks = vec![(b"IHDR", ihdr)];
        chunks.extend(extra.iter().cloned());
        chunks.push((b"IDAT", z.finish().unwrap()));
        chunks.push((b"IEND", Vec::new()));
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in chunks {
            out.extend((body.len() as u32).to_be_bytes());
            out.extend(kind);
            out.extend(&body);
            out.extend([0; 4]);
        }
        out
    }

    #[test]
    fn png_grey_alpha_with_filters() {
        // Row 0 unfiltered; row 1 "up" filter adding 0 to row 0.
        let data = png_file(2, 2, 8, 4, &[], &[0, 0, 255, 0, 0, 2, 0, 0, 0, 0]);
        assert_eq!(pixels(&decode(Kind::Png, 2, 2, &[], &data).unwrap()), [rgb(0, 0, 0), rgb(255, 255, 255), rgb(0, 0, 0), rgb(255, 255, 255)]);
    }

    #[test]
    fn png_palette_2_bit_and_paeth() {
        let plte = vec![255, 0, 0, 0, 255, 0, 0, 0, 255];
        // Indices 0, 1, 2, 1 packed MSB first.
        let data = png_file(4, 1, 2, 3, &[(b"PLTE", plte)], &[0, 0b0001_1001]);
        assert_eq!(pixels(&decode(Kind::Png, 4, 1, &[], &data).unwrap()), [rgb(255, 0, 0), rgb(0, 255, 0), rgb(0, 0, 255), rgb(0, 255, 0)]);
        // Paeth on RGB: second row predicts from the first.
        let data = png_file(1, 2, 8, 2, &[], &[0, 10, 20, 30, 4, 1, 1, 1]);
        assert_eq!(pixels(&decode(Kind::Png, 1, 2, &[], &data).unwrap()), [rgb(10, 20, 30), rgb(11, 21, 31)]);
    }

    #[test]
    fn group4_bilevel_and_mapped() {
        use fax::{encoder::Encoder, Color, VecWriter};
        let mut enc = Encoder::new(VecWriter::new());
        for y in 0..4 {
            enc.encode_line((0..8).map(|x| if x == y { Color::Black } else { Color::White }), 8).unwrap();
        }
        let data = enc.finish().unwrap().finish();
        match decode(Kind::Group4, 8, 4, &[], &data).unwrap() {
            Image::Bilevel(b) => assert!(b.get(2, 2) && !b.get(3, 2)),
            _ => panic!("expected bilevel"),
        }
        let px = pixels(&decode(Kind::Group4Mapped, 8, 4, &[0x0000FF, 0xFFFF00], &data).unwrap());
        assert_eq!(px[0], rgb(255, 255, 0));
        assert_eq!(px[1], rgb(0, 0, 255));
    }
}
