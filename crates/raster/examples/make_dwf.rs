//! Writes synthetic DWF files: `cargo run --release --example make_dwf -- DIR`.
//! `DIR/synthetic-classic.dwf` is a DWF 0.55 stream; `DIR/synthetic-package.dwf` a DWF 6 package
//! with two sheets. Both draw an A3 sheet in 1/100 mm with layers, colours, fills, arcs, text and an image,
//! and an "F" in the top-left corner that makes orientation errors obvious.

use std::io::Write;

/// Logical units per mm.
const UNITS: i32 = 100;

/// Binary W2D opcodes use relative coordinates; this tracks the current point.
struct W2d {
    out: Vec<u8>,
    at: [i32; 2],
}

impl W2d {
    fn new(header: &str) -> Self {
        W2d { out: header.as_bytes().to_vec(), at: [0; 2] }
    }

    fn ascii(&mut self, s: &str) {
        self.out.extend(s.as_bytes());
    }

    fn i32s(&mut self, v: &[i32]) {
        self.out.extend(v.iter().flat_map(|v| v.to_le_bytes()));
    }

    fn rel(&mut self, p: [i32; 2]) {
        self.i32s(&[p[0] - self.at[0], p[1] - self.at[1]]);
        self.at = p;
    }

    /// `p`: 32-bit relative polyline, or polygon while fill is on.
    fn poly(&mut self, points: &[[i32; 2]]) {
        self.out.push(b'p');
        self.out.push(points.len() as u8);
        points.iter().for_each(|&p| self.rel(p));
    }

    /// `r`: full circle.
    fn circle(&mut self, c: [i32; 2], r: i32) {
        self.out.push(b'r');
        self.rel(c);
        self.i32s(&[r]);
    }

    /// 0x92: circular arc, angles in 1/65536 turns.
    fn arc(&mut self, c: [i32; 2], r: i32, start: u16, end: u16) {
        self.out.push(0x92);
        self.rel(c);
        self.i32s(&[r]);
        self.out.extend(start.to_le_bytes());
        self.out.extend(end.to_le_bytes());
    }

    /// `x`: text at the baseline start, in the current font.
    fn text(&mut self, at: [i32; 2], s: &str) {
        self.out.push(b'x');
        self.rel(at);
        self.ascii(&format!("'{s}'"));
    }

    /// `{` zlib section `}`.
    fn compressed(&mut self, body: &[u8]) {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(body).unwrap();
        self.out.push(b'{');
        self.i32s(&[0]);
        self.out.extend(0x0011u16.to_le_bytes());
        self.out.extend(z.finish().unwrap());
        self.out.push(b'}');
    }
}

fn mm(x: i32, y: i32) -> [i32; 2] {
    [x * UNITS, y * UNITS]
}

/// The main test sheet.
fn sheet(header: &str, plot_info: bool) -> Vec<u8> {
    let mut w = W2d::new(header);
    if plot_info {
        w.ascii("(PlotInfo show 0 mm 420 297 0 0 0 0 ((0.01 0 0)(0 0.01 0)(0 0 1)))");
    }
    w.ascii("(Title 'Synthetic DWF')(Creator 'make_dwf')");
    w.ascii("(Layer 1 'Border')(LineWeight 50)");
    w.poly(&[mm(10, 10), mm(410, 10), mm(410, 287), mm(10, 287), mm(10, 10)]);
    w.ascii("(LineWeight 0)");
    // "F" at the top left.
    w.poly(&[mm(30, 230), mm(30, 270), mm(55, 270)]);
    w.poly(&[mm(30, 250), mm(48, 250)]);

    w.ascii("(Layer 2 'Geometry')(Color 255,0,0,255)");
    w.circle(mm(120, 150), 40 * UNITS);
    w.arc(mm(120, 150), 50 * UNITS, 0, 0x4000);
    w.ascii("(Color 0,0,255,255)F");
    w.poly(&[mm(200, 100), mm(260, 100), mm(230, 160)]);
    w.ascii("f(Color 0,160,0,255)");
    w.ascii(&format!(
        "(Contour 2 4 4 {} {} {} {} {} {} {} {})",
        p(mm(300, 100)), p(mm(380, 100)), p(mm(380, 180)), p(mm(300, 180)),
        p(mm(320, 120)), p(mm(360, 120)), p(mm(360, 160)), p(mm(320, 160))
    ));
    w.ascii(&format!("(Color 128,0,128,255)(Ellipse {} {},{} 0,65536 8192)", p(mm(230, 220)), 40 * UNITS, 15 * UNITS));
    // Hidden geometry must not show.
    w.ascii("v");
    w.circle(mm(210, 148), 100 * UNITS);
    w.ascii("V");

    w.ascii("(Layer 3 'Text')(Color 0,0,0,255)(Font (Name 'Arial')(Height 800))");
    w.text(mm(30, 30), "Synthetic DWF sheet");
    w.ascii("(Font (Height 400)(Rotation 16384))");
    w.text(mm(400, 40), "Rotated 90");
    w.ascii("(Font (Rotation 0))");

    // A 64 × 32 RGB gradient (opcode 0x0006), corners relative.
    let (cols, rows) = (64u16, 32u16);
    let pixels: Vec<u8> = (0..rows).flat_map(|y| (0..cols).flat_map(move |x| [(x * 4) as u8, (y * 8) as u8, 160])).collect();
    let mut payload = Vec::new();
    payload.extend(cols.to_le_bytes());
    payload.extend(rows.to_le_bytes());
    let mut corners = W2d::new("");
    corners.at = w.at;
    corners.rel(mm(300, 220));
    corners.rel(mm(380, 260));
    w.at = corners.at;
    payload.extend(corners.out);
    payload.extend(1i32.to_le_bytes()); // identifier
    payload.extend((pixels.len() as i32).to_le_bytes());
    payload.extend(pixels);
    payload.push(b'}');
    w.out.push(b'{');
    w.i32s(&[2 + payload.len() as i32]);
    w.out.extend(0x0006u16.to_le_bytes());
    w.out.extend(payload);

    let mut inner = W2d::new("");
    inner.at = w.at;
    inner.ascii("(Layer 4 'Compressed')(Color 0,128,255,255)");
    for k in 0..10 {
        inner.poly(&[mm(30 + 10 * k, 60), mm(30 + 10 * k, 100)]);
    }
    w.compressed(&inner.out);
    w.at = inner.at;
    w.ascii("(EndOfDWF)");
    w.out
}

fn p([x, y]: [i32; 2]) -> String {
    format!("{x},{y}")
}

/// A simple second sheet: a grid.
fn grid_sheet() -> Vec<u8> {
    let mut w = W2d::new("(W2D V06.00)");
    w.ascii("(Layer 1 'Grid')");
    for k in 0..=14 {
        w.poly(&[mm(10 + 20 * k, 10), mm(10 + 20 * k, 197)]);
    }
    for k in 0..=11 {
        w.poly(&[mm(10, 10 + 17 * k), mm(290, 10 + 17 * k)]);
    }
    w.ascii("(Font (Height 600))");
    w.text(mm(20, 200), "Sheet 2");
    w.out
}

/// A ZIP archive of stored entries.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let common = |v: &mut Vec<u8>| {
            v.extend(20u16.to_le_bytes());
            v.extend([0; 4]); // flags, method: stored
            v.extend([0; 4]); // time, date
            v.extend(crc.to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes());
            v.extend((data.len() as u32).to_le_bytes());
            v.extend((name.len() as u16).to_le_bytes());
            v.extend([0; 2]);
        };
        out.extend(0x0403_4B50u32.to_le_bytes());
        common(&mut out);
        out.extend(name.as_bytes());
        out.extend(*data);
        central.extend(0x0201_4B50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        common(&mut central);
        central.extend([0; 10]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4B50u32.to_le_bytes());
    out.extend([0; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend([0; 2]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { crc >> 1 ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn package() -> Vec<u8> {
    let section = |id: &str, title: &str| {
        format!(r#"<dwf:Section type="com.autodesk.dwf.ePlot" title="{title}" name="com.autodesk.dwf.ePlot_{id}"><dwf:Resource role="descriptor" mime="text/xml" href="com.autodesk.dwf.ePlot_{id}\descriptor.xml"/></dwf:Section>"#)
    };
    let manifest = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><dwf:Manifest xmlns:dwf="DWF-Manifest:6.0" version="6.0"><dwf:Sections>{}{}</dwf:Sections></dwf:Manifest>"#,
        section("A", "Plan"),
        section("B", "Grid")
    );
    let descriptor = |id: &str, w: u32, h: u32| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><ePlot:Page xmlns:ePlot="DWF-ePlot:1.2" xmlns:dwf="DWF-Manifest:6.0" version="1.2"><ePlot:Paper units="mm" width="{w}" height="{h}" color="255 255 255"/><dwf:Resources><ePlot:GraphicResource role="2d streaming graphics" mime="application/x-w2d" href="com.autodesk.dwf.ePlot_{id}\{id}.w2d" transform="0.01 0 0 0 0 0.01 0 0 0 0 1 0 0 0 0 1"/></dwf:Resources></ePlot:Page>"#
        )
    };
    let (da, db) = (descriptor("A", 420, 297), descriptor("B", 297, 210));
    let (ga, gb) = (sheet("(W2D V06.00)", false), grid_sheet());
    let entries: [(&str, &[u8]); 5] = [
        ("manifest.xml", manifest.as_bytes()),
        ("com.autodesk.dwf.ePlot_A\\descriptor.xml", da.as_bytes()),
        ("com.autodesk.dwf.ePlot_A\\A.w2d", &ga),
        ("com.autodesk.dwf.ePlot_B\\descriptor.xml", db.as_bytes()),
        ("com.autodesk.dwf.ePlot_B\\B.w2d", &gb),
    ];
    [b"(DWF V06.00)".as_slice(), &zip(&entries)].concat()
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "samples".into());
    let dir = std::path::Path::new(&dir);
    std::fs::write(dir.join("synthetic-classic.dwf"), sheet("(DWF V00.55)", true)).unwrap();
    std::fs::write(dir.join("synthetic-package.dwf"), package()).unwrap();
    println!("Wrote {}/synthetic-classic.dwf and synthetic-package.dwf", dir.display());
}
