//! Writes synthetic binary CGM files: `cargo run --release --example make_cgm -- DIR`.
//! `DIR/synthetic.cgm` has integer VDC in 1/10 mm (metric scaling) and two pictures: an A3 sheet
//! with WebCGM layers, colours, fills, arcs, text, a cell array and a Group 4 tile, then a small
//! second sheet. `DIR/synthetic-real.cgm` draws the same first sheet with 32-bit floating-point VDC
//! in millimetres and a downward Y axis. Both have an "F" in the top-left corner that makes
//! orientation errors obvious.

/// Element writer; coordinates are in mm and written as integer (1/10 mm) or real VDC.
struct Cgm {
    out: Vec<u8>,
    real: bool,
    /// Sheet height in mm, for flipping Y when the VDC axis points down.
    flip: Option<f64>,
}

impl Cgm {
    fn cmd(&mut self, class: u16, id: u16, data: &[u8]) {
        let head = class << 12 | id << 5;
        if data.len() < 31 {
            self.out.extend((head | data.len() as u16).to_be_bytes());
            self.out.extend(data);
        } else {
            self.out.extend((head | 31).to_be_bytes());
            for (k, part) in data.chunks(0x7FFE).enumerate() {
                let more = if (k + 1) * 0x7FFE < data.len() { 0x8000 } else { 0 };
                self.out.extend((more | part.len() as u16).to_be_bytes());
                self.out.extend(part);
            }
        }
        if data.len() % 2 == 1 {
            self.out.push(0);
        }
    }

    fn vdc(&self, v: f64) -> Vec<u8> {
        if self.real { (v as f32).to_be_bytes().to_vec() } else { ((v * 10.0).round() as i16).to_be_bytes().to_vec() }
    }

    fn points(&self, pts: &[[f64; 2]]) -> Vec<u8> {
        pts.iter().flat_map(|&[x, y]| [self.vdc(x), self.vdc(self.flip.map_or(y, |h| h - y))].concat()).collect()
    }

    fn polyline(&mut self, pts: &[[f64; 2]]) {
        let data = self.points(pts);
        self.cmd(4, 1, &data);
    }

    fn polygon(&mut self, pts: &[[f64; 2]]) {
        let data = self.points(pts);
        self.cmd(4, 7, &data);
    }

    fn circle(&mut self, c: [f64; 2], r: f64) {
        let data = [self.points(&[c]), self.vdc(r)].concat();
        self.cmd(4, 12, &data);
    }

    fn text(&mut self, at: [f64; 2], s: &str) {
        let data = [self.points(&[at]), e(1), string(s)].concat();
        self.cmd(4, 4, &data);
    }

    fn enumerated(&mut self, class: u16, id: u16, v: i16) {
        self.cmd(class, id, &e(v));
    }

    /// Opens a WebCGM layer.
    fn layer(&mut self, name: &str) {
        self.cmd(0, 21, &[string(name), string("layer"), e(0)].concat());
        let sdr = [e(14), e(1), string(name)].concat();
        self.cmd(9, 1, &[string("layername"), string(&String::from_utf8(sdr).unwrap())].concat());
        self.cmd(0, 22, &[]);
    }
}

fn e(v: i16) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

fn string(s: &str) -> Vec<u8> {
    [&[s.len() as u8][..], s.as_bytes()].concat()
}

/// An A3 sheet: border, title block, an "F", shapes on layers, text, a cell array and a G4 tile.
fn sheet(c: &mut Cgm) {
    // Colour table: 2 red, 3 blue, 4 light grey, 5 green.
    c.cmd(5, 34, &[2, 220, 30, 30, 30, 60, 200, 210, 210, 210, 20, 150, 60]);
    c.layer("Border");
    c.cmd(5, 3, &c.vdc(0.7));
    c.polyline(&[[10.0, 10.0], [410.0, 10.0], [410.0, 287.0], [10.0, 287.0], [10.0, 10.0]]);
    c.cmd(5, 3, &c.vdc(0.0));
    c.polyline(&[[290.0, 10.0], [290.0, 50.0], [410.0, 50.0]]);
    // "F" near the top-left: upright and readable when orientation is right.
    c.polyline(&[[30.0, 240.0], [30.0, 275.0], [50.0, 275.0]]);
    c.polyline(&[[30.0, 258.0], [45.0, 258.0]]);
    c.cmd(0, 23, &[]);

    c.layer("Shapes");
    c.enumerated(5, 22, 1);
    c.cmd(5, 23, &[4]);
    c.enumerated(5, 30, 1);
    c.cmd(5, 29, &[3]);
    c.polygon(&[[80.0, 150.0], [160.0, 150.0], [120.0, 220.0]]);
    // Polygon set: a square with a square hole, the hole's edges invisible.
    let square = |x: f64, y: f64, s: f64| [[x, y], [x + s, y], [x + s, y + s], [x, y + s]];
    let mut set = Vec::new();
    for (k, p) in square(190.0, 150.0, 70.0).iter().chain(&square(210.0, 170.0, 30.0)).enumerate() {
        let flag = match k {
            3 => 3,
            7 => 2,
            k if k < 4 => 1,
            _ => 0,
        };
        set.extend([c.points(&[*p]), e(flag)].concat());
    }
    c.cmd(4, 8, &set);
    c.enumerated(5, 22, 0);
    c.cmd(5, 23, &[2]);
    c.circle([330.0, 185.0], 35.0);
    // Pie: centre arc of 120°, filled green.
    c.enumerated(5, 22, 1);
    c.cmd(5, 23, &[5]);
    // Arcs run counter-clockwise in VDC, so with Y down the same wedge goes from 240° to 0°.
    let (start, end) = if c.flip.is_some() { ([-0.5, -0.866], [1.0, 0.0]) } else { ([1.0, 0.0], [-0.5, 0.866]) };
    let pie = [c.points(&[[330.0, 185.0]]), c.vdc(start[0]), c.vdc(start[1]), c.vdc(end[0]), c.vdc(end[1]), c.vdc(25.0), e(0)].concat();
    c.cmd(4, 16, &pie);
    c.enumerated(5, 22, 0);
    c.enumerated(5, 30, 0);
    c.cmd(5, 4, &[3]);
    // Ellipse and a 3-point arc.
    let ellipse = c.points(&[[120.0, 90.0], [170.0, 90.0], [120.0, 115.0]]);
    c.cmd(4, 17, &ellipse);
    let arc = c.points(&[[200.0, 60.0], [240.0, 100.0], [280.0, 60.0]]);
    c.cmd(4, 13, &arc);
    // Continuous Bézier.
    let bezier = [e(2), c.points(&[[300.0, 80.0], [320.0, 130.0], [360.0, 30.0], [390.0, 80.0]])].concat();
    c.cmd(4, 26, &bezier);
    c.cmd(0, 23, &[]);

    c.layer("Text");
    c.cmd(5, 14, &[3]);
    if c.flip.is_some() {
        // With Y down, "up" for characters is towards negative Y.
        c.cmd(5, 16, &[c.vdc(0.0), c.vdc(-1.0), c.vdc(1.0), c.vdc(0.0)].concat());
    }
    c.cmd(5, 15, &c.vdc(5.0));
    c.text([295.0, 35.0], "SYNTHETIC CGM");
    c.cmd(5, 15, &c.vdc(3.5));
    c.text([295.0, 20.0], "Sheet 1 of 2 - A3");
    c.cmd(0, 23, &[]);

    c.layer("Images");
    // 4 × 2 cell array of colour indices, first row along the top.
    let cells = [c.points(&[[30.0, 60.0], [70.0, 40.0], [70.0, 60.0]]), e(4), e(2), e(8), e(1), vec![2, 3, 4, 5, 5, 4, 3, 2]].concat();
    c.cmd(4, 9, &cells);
    // A 64 × 32 Group 4 tile with a diagonal cross.
    let (w, h) = (64u16, 32u16);
    let mut enc = fax::encoder::Encoder::new(fax::VecWriter::new());
    for y in 0..h {
        let ink = |x: u16| x / 2 == y || (w - 1 - x) / 2 == y || y == 0 || y == h - 1;
        enc.encode_line((0..w).map(|x| if ink(x) { fax::Color::Black } else { fax::Color::White }), w).unwrap();
    }
    let g4 = enc.finish().unwrap().finish();
    // REAL: 32-bit float in the real file, else the default 16.16 fixed point.
    let real = |v: f64| if c.real { (v as f32).to_be_bytes().to_vec() } else { [e(v as i16), e((v.fract() * 65536.0) as u16 as i16)].concat() };
    // 0.5 mm cells, in VDC units.
    let cell_size = if c.real { 0.5 } else { 5.0 };
    let line_dir = if c.flip.is_some() { 0 } else { 1 };
    let begin = [c.points(&[[90.0, 60.0]]), e(0), e(line_dir), e(1), e(1), e(w as i16), e(h as i16), real(cell_size), real(cell_size), e(0), e(0), e(w as i16), e(h as i16)].concat();
    c.cmd(0, 19, &begin);
    c.cmd(4, 28, &[e(2), e(0), vec![0, 3], vec![0], g4].concat());
    c.cmd(0, 20, &[]);
    c.cmd(0, 23, &[]);
}

fn metafile(real: bool) -> Vec<u8> {
    let mut c = Cgm { out: Vec::new(), real, flip: real.then_some(297.0) };
    c.cmd(0, 1, &string("Synthetic"));
    c.cmd(1, 1, &e(1));
    c.cmd(1, 2, &string("Synthetic test file written by make_cgm"));
    c.cmd(1, 13, &[string("Helvetica"), string("Courier")].concat());
    if real {
        c.enumerated(1, 3, 1);
        // REAL PRECISION: 32-bit floating point (used for the tile cell size).
        c.cmd(1, 5, &[e(0), e(9), e(23)].concat());
        // VDC REAL PRECISION in the defaults.
        let mut inner = Cgm { out: Vec::new(), real, flip: None };
        inner.cmd(3, 2, &[e(0), e(9), e(23)].concat());
        c.cmd(1, 12, &inner.out);
    }
    c.cmd(0, 3, &string("Sheet 1"));
    // Metric: 1 VDC unit is 0.1 mm (integer) or 1 mm (real).
    let factor: f32 = if real { 1.0 } else { 0.1 };
    c.cmd(2, 1, &[e(1), factor.to_be_bytes().to_vec()].concat());
    let extent = if real { [c.vdc(0.0), c.vdc(297.0), c.vdc(420.0), c.vdc(0.0)].concat() } else { c.points(&[[0.0, 0.0], [420.0, 297.0]]) };
    c.cmd(2, 6, &extent);
    c.enumerated(2, 3, 0);
    c.cmd(0, 4, &[]);
    sheet(&mut c);
    c.cmd(0, 5, &[]);
    if !real {
        c.cmd(0, 3, &string("Sheet 2"));
        c.cmd(2, 1, &[e(1), factor.to_be_bytes().to_vec()].concat());
        c.cmd(0, 4, &[]);
        c.polyline(&[[0.0, 0.0], [100.0, 0.0], [100.0, 50.0], [0.0, 50.0], [0.0, 0.0]]);
        c.cmd(5, 15, &c.vdc(5.0));
        c.text([10.0, 20.0], "Sheet 2");
        c.cmd(0, 5, &[]);
    }
    c.cmd(0, 2, &[]);
    c.out
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "samples".into());
    std::fs::create_dir_all(&dir).unwrap();
    for (name, real) in [("synthetic.cgm", false), ("synthetic-real.cgm", true)] {
        let path = std::path::Path::new(&dir).join(name);
        std::fs::write(&path, metafile(real)).unwrap();
        println!("{}", path.display());
    }
}
