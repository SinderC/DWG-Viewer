//! TIFF, decoded with the `tiff` crate. Bilevel pages become a [`Bitmap`]; grey and colour
//! pages become an [`RgbaImage`].

use std::io::Cursor;

use tiff::decoder::ifd::Value;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;
use tiff::{ColorType, TiffError};

use crate::bitmap::Bitmap;
use crate::orient;
use crate::rgba::{rgb, RgbaImage, MAX_PIXELS};

pub enum Image {
    Bilevel(Bitmap),
    Color(RgbaImage),
}

type TiffDecoder<'a> = Decoder<Cursor<&'a [u8]>>;

fn err(e: TiffError) -> String {
    format!("TIFF: {e}")
}

fn open(data: &[u8]) -> Result<TiffDecoder<'_>, String> {
    let mut limits = Limits::default();
    // Colour size is checked against MAX_PIXELS before decoding; allow up to 16-bit RGBA.
    limits.decoding_buffer_size = MAX_PIXELS as usize * 8;
    limits.intermediate_buffer_size = limits.decoding_buffer_size;
    Ok(Decoder::new(Cursor::new(data)).map_err(err)?.with_limits(limits))
}

/// IFD indices of the pages, skipping reduced-resolution images such as scanner thumbnails.
fn pages(dec: &mut TiffDecoder) -> Result<Vec<usize>, String> {
    let mut pages = Vec::new();
    for ifd in 0.. {
        let subfile: u32 = dec.find_tag_unsigned(Tag::NewSubfileType).map_err(err)?.unwrap_or(0);
        if subfile & 1 == 0 {
            pages.push(ifd);
        }
        if !dec.more_images() {
            break;
        }
        dec.next_image().map_err(err)?;
    }
    Ok(pages)
}

pub fn page_count(data: &[u8]) -> Result<u32, String> {
    Ok(pages(&mut open(data)?)?.len() as u32)
}

/// A decoder positioned at page `page` (0-based).
fn open_page(data: &[u8], page: u32) -> Result<TiffDecoder<'_>, String> {
    let mut dec = open(data)?;
    let ifd = *pages(&mut dec)?.get(page as usize).ok_or(format!("TIFF has no page {}", page + 1))?;
    dec.seek_to_image(ifd).map_err(err)?;
    Ok(dec)
}

/// Decodes page `page` (0-based) into an upright image with square pixels, plus its horizontal
/// resolution in dpi (0 if unknown).
pub fn decode(data: &[u8], page: u32) -> Result<(Image, u32), String> {
    let mut dec = open_page(data, page)?;

    let (width, height) = dec.dimensions().map_err(err)?;
    let color = dec.colortype().map_err(err)?;
    let orientation = dec.find_tag_unsigned(Tag::Orientation).map_err(err)?.unwrap_or(1);
    let dpi = dpi(&mut dec)?;
    let rows = square_rows(&mut dec, height);
    let (pel_path, line_prog) = orient::from_tiff(orientation);

    if color == ColorType::Gray(1) {
        let DecodingResult::U8(bytes) = dec.read_image().map_err(err)? else { unreachable!() };
        let b = bilevel(&bytes, width, height);
        let bitmap = Bitmap { data: resample_rows(b.data, b.words_per_row, height, rows), height: rows, ..b };
        return Ok((Image::Bilevel(bitmap.oriented(pel_path, line_prog)), dpi));
    }

    if width as u64 * rows as u64 > MAX_PIXELS {
        return Err(format!("Image too large ({width}×{rows}); colour images are limited to {} megapixels", MAX_PIXELS / 1_000_000));
    }
    // 8-bit samples; 16-bit ones keep their high byte.
    let samples: Vec<u8> = match dec.read_image().map_err(err)? {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => v.iter().map(|s| (s >> 8) as u8).collect(),
        _ => return Err(format!("Unsupported TIFF sample format ({color:?})")),
    };
    let convert: fn(&[u8]) -> u32 = match color {
        ColorType::Gray(8 | 16) => |s| rgb(s[0], s[0], s[0]),
        // Grey with alpha is reported as two-sample Multiband.
        ColorType::Multiband { bit_depth: 8 | 16, num_samples: 2 } => |s| {
            let g = over_white(s[0], s[1]);
            rgb(g, g, g)
        },
        ColorType::RGB(8 | 16) => |s| rgb(s[0], s[1], s[2]),
        ColorType::RGBA(8 | 16) => |s| rgb(over_white(s[0], s[3]), over_white(s[1], s[3]), over_white(s[2], s[3])),
        ColorType::CMYK(8 | 16) => |s| {
            let c = |v: u8| ((255 - v as u32) * (255 - s[3] as u32) / 255) as u8;
            rgb(c(s[0]), c(s[1]), c(s[2]))
        },
        ColorType::YCbCr(8) => ycbcr,
        _ => return Err(format!("Unsupported TIFF colour type {color:?}")),
    };
    let n = samples.len() / (width as usize * height as usize);
    let data: Vec<u32> = samples.chunks_exact(n).map(convert).collect();
    let data = resample_rows(data, width as usize, height, rows);
    let image = RgbaImage { width, height: rows, data }.oriented(pel_path, line_prog);
    Ok((Image::Color(image), dpi))
}

/// Unpacks MSB-first 1-bit rows (padded to whole bytes, 0 = black) into a bitmap.
fn bilevel(bytes: &[u8], width: u32, height: u32) -> Bitmap {
    let mut bitmap = Bitmap::new(width, height);
    let stride = (width as usize).div_ceil(8);
    let tail = width % 64;
    for (y, src) in bytes.chunks_exact(stride).take(height as usize).enumerate() {
        let row = bitmap.row_mut(y as u32);
        for (word, chunk) in row.iter_mut().zip(src.chunks(8)) {
            let mut le = [0xFF; 8];
            for (d, s) in le.iter_mut().zip(chunk) {
                *d = s.reverse_bits();
            }
            *word = !u64::from_le_bytes(le);
        }
        if tail != 0 {
            *row.last_mut().unwrap() &= (1 << tail) - 1;
        }
    }
    bitmap
}

fn over_white(c: u8, alpha: u8) -> u8 {
    (c as u32 * alpha as u32 / 255 + 255 - alpha as u32) as u8
}

/// JPEG-compressed colour TIFFs are decoded as YCbCr (JFIF, full range).
fn ycbcr(s: &[u8]) -> u32 {
    let (y, cb, cr) = (s[0] as f32, s[1] as f32 - 128.0, s[2] as f32 - 128.0);
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    rgb(c(y + 1.402 * cr), c(y - 0.344136 * cb - 0.714136 * cr), c(y + 1.772 * cb))
}

/// The row count that makes pixels square: fax images have a lower vertical than horizontal
/// resolution (204 × 98 dpi in standard mode), so their rows are stretched. Unchanged if either
/// resolution is missing or the ratio is implausible.
fn square_rows(dec: &mut TiffDecoder, height: u32) -> u32 {
    let mut resolution = |tag| match dec.find_tag(tag) {
        Ok(Some(Value::Rational(n, d))) if n > 0 && d > 0 => Some(n as f64 / d as f64),
        _ => None,
    };
    let (Some(x), Some(y)) = (resolution(Tag::XResolution), resolution(Tag::YResolution)) else { return height };
    let ratio = x / y;
    if (ratio - 1.0).abs() < 0.01 || !(0.125..=8.0).contains(&ratio) {
        return height;
    }
    ((height as f64 * ratio).round() as u32).max(1)
}

/// Nearest-neighbour resampling of a row-major buffer (`stride` elements per row) from `height` to `rows` rows.
fn resample_rows<T: Copy>(data: Vec<T>, stride: usize, height: u32, rows: u32) -> Vec<T> {
    if rows == height {
        return data;
    }
    (0..rows as u64).flat_map(|y| data[(y * height as u64 / rows as u64) as usize * stride..][..stride].iter().copied()).collect()
}

fn dpi(dec: &mut TiffDecoder) -> Result<u32, String> {
    let Some(Value::Rational(n, d)) = dec.find_tag(Tag::XResolution).map_err(err)? else { return Ok(0) };
    if d == 0 {
        return Ok(0);
    }
    let per_unit = n as f64 / d as f64;
    Ok(match dec.find_tag_unsigned(Tag::ResolutionUnit).map_err(err)?.unwrap_or(2u16) {
        2 => per_unit.round() as u32,
        3 => (per_unit * 2.54).round() as u32,
        _ => 0,
    })
}

/// Tags of page `page` (0-based) as (label, value) rows for display. Unreadable tags are left out.
pub fn info(data: &[u8], page: u32) -> Result<Vec<(String, String)>, String> {
    let mut dec = open_page(data, page)?;
    let mut rows = Vec::new();
    let mut row = |label: &str, value: String| rows.push((label.to_string(), value));

    let big = matches!(data.get(2..4), Some(b"+\0" | b"\0+"));
    let order = if data.starts_with(b"II") { "little-endian" } else { "big-endian" };
    row("Variant", format!("{}, {order}", if big { "BigTIFF" } else { "TIFF" }));
    let compression = unsigned(&mut dec, Tag::Compression).unwrap_or(1);
    row("Compression", named(compression, COMPRESSION));
    if let Some(p) = unsigned(&mut dec, Tag::PhotometricInterpretation) {
        row("Photometric", named(p, PHOTOMETRIC));
    }
    if let Some(bits) = dec.find_tag_unsigned_vec::<u32>(Tag::BitsPerSample).ok().flatten() {
        row("Bits per sample", bits.iter().map(u32::to_string).collect::<Vec<_>>().join(", "));
    }
    if let Some(n) = unsigned(&mut dec, Tag::SamplesPerPixel) {
        row("Samples per pixel", n.to_string());
    }
    if let Some(p) = unsigned(&mut dec, Tag::Predictor) {
        row("Predictor", named(p, &[(1, "None"), (2, "Horizontal differencing"), (3, "Floating point")]));
    }
    if let Some(p) = unsigned(&mut dec, Tag::PlanarConfiguration) {
        row("Planar configuration", named(p, &[(1, "Chunky"), (2, "Planar")]));
    }
    match (unsigned(&mut dec, Tag::TileWidth), unsigned(&mut dec, Tag::TileLength), unsigned(&mut dec, Tag::RowsPerStrip)) {
        (Some(w), Some(h), _) => row("Layout", format!("Tiles of {w} × {h}")),
        // RowsPerStrip may exceed the height (often 2³²−1): one strip.
        (_, _, Some(rows)) if rows >= dec.dimensions().map_or(0, |(_, h)| h) => row("Layout", "Single strip".into()),
        (_, _, Some(rows)) => row("Layout", format!("Strips of {rows} rows")),
        _ => {}
    }
    if let Some(o) = unsigned(&mut dec, Tag::Orientation) {
        row("Orientation", named(o, ORIENTATION));
    }
    let unit = unsigned(&mut dec, Tag::ResolutionUnit).unwrap_or(2);
    let rational = |v: Option<Value>| match v {
        Some(Value::Rational(n, d)) if d != 0 => Some(n as f64 / d as f64),
        _ => None,
    };
    let x = rational(dec.find_tag(Tag::XResolution).ok().flatten());
    let y = rational(dec.find_tag(Tag::YResolution).ok().flatten());
    if let (Some(x), Some(y)) = (x, y) {
        let unit = match unit {
            2 => " dpi",
            3 => " pixels/cm",
            _ => " (no unit)",
        };
        // Usually square; fax images are not (204 × 98 dpi in standard mode).
        let value = if decimal2(x) == decimal2(y) { decimal2(x) } else { format!("{} × {}", decimal2(x), decimal2(y)) };
        row("Resolution", format!("{value}{unit}"));
    }
    let text_tags = [
        ("Description", Tag::ImageDescription),
        ("Make", Tag::Make),
        ("Model", Tag::Model),
        ("Software", Tag::Software),
        ("Date", Tag::DateTime),
        ("Artist", Tag::Artist),
        ("Host computer", Tag::HostComputer),
        ("Copyright", Tag::Copyright),
    ];
    for (label, tag) in text_tags {
        if let Ok(Some(Value::Ascii(s))) = dec.find_tag(tag) {
            let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            if !s.is_empty() {
                row(label, s.to_string());
            }
        }
    }
    Ok(rows)
}

const COMPRESSION: &[(u32, &str)] = &[
    (1, "None"),
    (2, "CCITT modified Huffman (RLE)"),
    (3, "CCITT Group 3"),
    (4, "CCITT Group 4"),
    (5, "LZW"),
    (6, "JPEG (old-style)"),
    (7, "JPEG"),
    (8, "Deflate"),
    (32773, "PackBits"),
    (32946, "Deflate (PKZIP)"),
    (34925, "LZMA"),
    (50000, "Zstandard"),
    (50001, "WebP"),
];

const PHOTOMETRIC: &[(u32, &str)] = &[
    (0, "White is zero"),
    (1, "Black is zero"),
    (2, "RGB"),
    (3, "Palette"),
    (4, "Transparency mask"),
    (5, "CMYK"),
    (6, "YCbCr"),
    (8, "CIE L*a*b*"),
];

const ORIENTATION: &[(u32, &str)] = &[
    (1, "Top-left (normal)"),
    (2, "Top-right (mirrored)"),
    (3, "Bottom-right (rotated 180°)"),
    (4, "Bottom-left (flipped)"),
    (5, "Left-top (transposed)"),
    (6, "Right-top (rotated 90° CW)"),
    (7, "Right-bottom (transverse)"),
    (8, "Left-bottom (rotated 90° CCW)"),
];

fn unsigned(dec: &mut TiffDecoder, tag: Tag) -> Option<u32> {
    dec.find_tag_unsigned(tag).ok().flatten()
}

/// The name of `value` from `names`, or the bare number.
fn named(value: u32, names: &[(u32, &str)]) -> String {
    names.iter().find(|(v, _)| *v == value).map_or(value.to_string(), |(_, name)| name.to_string())
}

/// `v` with up to two decimals. Formats integers only: `f64` Display adds ~25 KB to the WASM.
fn decimal2(v: f64) -> String {
    let hundredths = (v * 100.0).round() as u64;
    let (whole, frac) = (hundredths / 100, hundredths % 100);
    match frac {
        0 => whole.to_string(),
        f if f % 10 == 0 => format!("{whole}.{}", f / 10),
        f => format!("{whole}.{f:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::encoder::{colortype, Compression, TiffEncoder};

    /// Width, height, SHORT tags and uncompressed data.
    type Page<'a> = (u32, u32, &'a [(u16, u16)], &'a [u8]);

    /// Little-endian TIFF with one strip per page; width, height, strip offset and byte count
    /// tags are added.
    fn build(pages: &[Page]) -> Vec<u8> {
        let mut out = b"II*\0\0\0\0\0".to_vec();
        let mut next_ptr = 4;
        for &(width, height, tags, data) in pages {
            let data_pos = out.len() as u32;
            out.extend(data);
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let ifd_pos = out.len() as u32;
            out[next_ptr..next_ptr + 4].copy_from_slice(&ifd_pos.to_le_bytes());
            let mut entries: Vec<(u16, u16, u32)> = tags.iter().map(|&(t, v)| (t, 3, v as u32)).collect();
            entries.extend([(256, 4, width), (257, 4, height), (273, 4, data_pos), (279, 4, data.len() as u32)]);
            entries.sort();
            out.extend((entries.len() as u16).to_le_bytes());
            for (tag, ty, value) in entries {
                out.extend(tag.to_le_bytes());
                out.extend(ty.to_le_bytes());
                out.extend(1u32.to_le_bytes());
                out.extend(value.to_le_bytes());
            }
            next_ptr = out.len();
            out.extend([0; 4]);
        }
        out
    }

    const PHOTOMETRIC: u16 = 262;
    const BITS: u16 = 258;
    const SUBFILE: u16 = 254;
    const ORIENTATION: u16 = 274;

    /// 10×2 bilevel, BlackIsZero: row 0 is `X.X.......`, row 1 is `.........X`.
    const BLACK_IS_ZERO: &[u8] = &[0b0101_1111, 0b1100_0000, 0b1111_1111, 0b1000_0000];

    fn pixels(b: &Bitmap) -> Vec<String> {
        (0..b.height).map(|y| (0..b.width).map(|x| if b.get(x, y) { 'X' } else { '.' }).collect()).collect()
    }

    fn bilevel_page(data: &[u8]) -> Bitmap {
        match decode(data, 0).unwrap().0 {
            Image::Bilevel(b) => b,
            Image::Color(_) => panic!("expected bilevel"),
        }
    }

    #[test]
    fn g4_roundtrip() {
        use fax::{encoder::Encoder, Color, VecWriter};
        let (w, h) = (70u32, 23u32);
        let black = |x: u32, y: u32| (x * 7 + y * 3) % 11 < 4 || x == y;
        let mut enc = Encoder::new(VecWriter::new());
        for y in 0..h {
            enc.encode_line((0..w).map(|x| if black(x, y) { Color::Black } else { Color::White }), w as u16).unwrap();
        }
        let file = fax::tiff::wrap(&enc.finish().unwrap().finish(), w, h);
        let (image, dpi) = decode(&file, 0).unwrap();
        assert_eq!(dpi, 200);
        let Image::Bilevel(bmp) = image else { panic!("expected bilevel") };
        for y in 0..h {
            for x in 0..w {
                assert_eq!(bmp.get(x, y), black(x, y), "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn uncompressed_black_is_zero() {
        let file = build(&[(10, 2, &[(BITS, 1), (PHOTOMETRIC, 1)], BLACK_IS_ZERO)]);
        assert_eq!(pixels(&bilevel_page(&file)), ["X.X.......", ".........X"]);
    }

    #[test]
    fn decimal2_formats() {
        assert_eq!([decimal2(400.0), decimal2(157.48), decimal2(72.5), decimal2(0.049)], ["400", "157.48", "72.5", "0.05"]);
    }

    #[test]
    fn fax_resolution_stretches_rows() {
        use tiff::encoder::Rational;
        let mut file = Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut file).unwrap();
        let mut image = enc.new_image::<colortype::Gray8>(2, 2).unwrap();
        image.resolution_unit(tiff::tags::ResolutionUnit::Inch);
        image.x_resolution(Rational { n: 204, d: 1 });
        image.y_resolution(Rational { n: 98, d: 1 });
        image.write_data(&[0, 0, 255, 255]).unwrap();
        let img = color_page(file.get_ref());
        // 2 rows × 204/98 → 4 rows, each source row repeated.
        let (black, white) = (rgb(0, 0, 0), rgb(255, 255, 255));
        assert_eq!((img.height, img.data), (4, vec![black, black, black, black, white, white, white, white]));
        let rows = info(file.get_ref(), 0).unwrap();
        assert!(rows.iter().any(|(l, v)| l == "Resolution" && v == "204 × 98 dpi"), "{rows:?}");
    }

    #[test]
    fn info_rows() {
        let file = build(&[(10, 2, &[(BITS, 1), (PHOTOMETRIC, 0)], &[0; 4])]);
        let rows = info(&file, 0).unwrap();
        let get = |label: &str| rows.iter().find(|(l, _)| l == label).map(|(_, v)| v.as_str());
        assert_eq!(get("Variant"), Some("TIFF, little-endian"));
        assert_eq!(get("Compression"), Some("None"));
        assert_eq!(get("Photometric"), Some("White is zero"));
        assert_eq!(get("Bits per sample"), Some("1"));
    }

    #[test]
    fn white_is_zero() {
        let inverted: Vec<u8> = BLACK_IS_ZERO.iter().map(|b| !b).collect();
        let file = build(&[(10, 2, &[(BITS, 1), (PHOTOMETRIC, 0)], &inverted)]);
        assert_eq!(pixels(&bilevel_page(&file)), ["X.X.......", ".........X"]);
    }

    #[test]
    fn orientation_6_rotates_clockwise() {
        // 3×2 `X..` / `XX.` (BlackIsZero bytes) displayed rotated 90° clockwise.
        let file = build(&[(3, 2, &[(BITS, 1), (PHOTOMETRIC, 1), (ORIENTATION, 6)], &[0b0111_1111, 0b0011_1111])]);
        assert_eq!(pixels(&bilevel_page(&file)), ["XX", "X.", ".."]);
    }

    #[test]
    fn pages_skip_thumbnails() {
        let bilevel: &[(u16, u16)] = &[(BITS, 1), (PHOTOMETRIC, 1)];
        let thumb: &[(u16, u16)] = &[(BITS, 1), (PHOTOMETRIC, 1), (SUBFILE, 1)];
        let file = build(&[(10, 2, bilevel, BLACK_IS_ZERO), (8, 1, thumb, &[0]), (8, 1, bilevel, &[0b1111_1110])]);
        assert_eq!(page_count(&file).unwrap(), 2);
        let (Image::Bilevel(page2), _) = decode(&file, 1).unwrap() else { panic!("expected bilevel") };
        assert_eq!(pixels(&page2), [".......X"]);
        assert!(decode(&file, 2).err().unwrap().contains("no page 3"));
    }

    fn color_page(data: &[u8]) -> RgbaImage {
        match decode(data, 0).unwrap().0 {
            Image::Color(c) => c,
            Image::Bilevel(_) => panic!("expected colour"),
        }
    }

    #[test]
    fn gray8_lzw_and_rgb8_deflate() {
        let mut file = Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut file).unwrap().with_compression(Compression::Lzw);
        enc.write_image::<colortype::Gray8>(2, 1, &[0, 200]).unwrap();
        let img = color_page(file.get_ref());
        assert_eq!(img.data, [rgb(0, 0, 0), rgb(200, 200, 200)]);

        let mut file = Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut file).unwrap().with_compression(Compression::Deflate(Default::default()));
        enc.write_image::<colortype::RGB8>(1, 2, &[1, 2, 3, 250, 128, 0]).unwrap();
        let img = color_page(file.get_ref());
        assert_eq!((img.width, img.height, img.data), (1, 2, vec![rgb(1, 2, 3), rgb(250, 128, 0)]));
    }

    #[test]
    fn rejects_non_tiff() {
        assert!(page_count(b"not a tiff file").is_err());
        assert!(decode(&[0; 100], 0).is_err());
    }
}
