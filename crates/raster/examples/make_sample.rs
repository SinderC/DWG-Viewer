//! Writes a synthetic CALS Type 1 drawing: `cargo run --release --example make_sample -- out.cal WIDTH HEIGHT [RORIENT]`.
//! With a non-default RORIENT the pixels are stored in that orientation, so a viewer should
//! display the same upright image for every orientation.

use fax::{encoder::Encoder, Color, VecWriter};

fn black(x: i64, y: i64, w: i64, h: i64) -> bool {
    let border = x < 40 || y < 40 || x >= w - 40 || y >= h - 40;
    let grid = (x % 500 < 3) || (y % 500 < 3);
    let (dx, dy) = (x - w / 2, y - h / 2);
    let r = ((dx * dx + dy * dy) as f64).sqrt();
    let circles = (r % 400.0) < 2.0;
    let diagonal = (x * h - y * w).abs() < 2 * w.max(h);
    // An "F" in the top-left corner makes orientation errors obvious.
    let f = (100..140).contains(&x) && (100..400).contains(&y)
        || (100..300).contains(&x) && (100..140).contains(&y)
        || (100..240).contains(&x) && (230..270).contains(&y);
    border || grid || circles || diagonal || f
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let (w, h): (i64, i64) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let orient = args.get(4).map_or("000,270", String::as_str);
    // Inverse of Bitmap::oriented: stored (sx, sy) -> upright (x, y).
    let (sw, sh, to_upright): (i64, i64, Box<dyn Fn(i64, i64) -> (i64, i64)>) = match orient {
        "000,270" => (w, h, Box::new(|sx, sy| (sx, sy))),
        "090,270" => (h, w, Box::new(move |sx, sy| (sy, h - 1 - sx))),
        "180,270" => (w, h, Box::new(move |sx, sy| (w - 1 - sx, h - 1 - sy))),
        "270,270" => (h, w, Box::new(move |sx, sy| (w - 1 - sy, sx))),
        _ => panic!("unsupported orientation {orient}"),
    };

    let mut header = Vec::new();
    for rec in ["srcdocid: SAMPLE", "rtype: 1", &format!("rorient: {orient}"), &format!("rpelcnt: {sw:06},{sh:06}"), "rdensty: 0400", "notes: synthetic test drawing"] {
        let mut rec = rec.as_bytes().to_vec();
        rec.resize(128, b' ');
        header.extend(rec);
    }
    header.resize(2048, b' ');

    let mut enc = Encoder::new(VecWriter::new());
    for sy in 0..sh {
        let pels = (0..sw).map(|sx| {
            let (x, y) = to_upright(sx, sy);
            if black(x, y, w, h) { Color::Black } else { Color::White }
        });
        enc.encode_line(pels, sw as u16).unwrap();
    }
    header.extend(enc.finish().unwrap().finish());
    std::fs::write(path, header).unwrap();
}
