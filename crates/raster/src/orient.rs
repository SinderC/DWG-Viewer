//! Image orientation as CALS `rorient` angles: pel path angle and line progression angle
//! (relative to the pel path), both counter-clockwise in degrees. `000,270` is upright.

/// Maps stored pixel coordinates to upright ones.
pub struct Transform {
    p: (i64, i64),
    q: (i64, i64),
    off: (i64, i64),
    pub width: u32,
    pub height: u32,
}

impl Transform {
    /// `None` for the identity, so callers can keep the image as is.
    pub fn new(width: u32, height: u32, pel_path: u32, line_prog: u32) -> Option<Self> {
        let dir = |deg: u32| -> (i64, i64) {
            match deg % 360 {
                0 => (1, 0),
                90 => (0, -1),
                180 => (-1, 0),
                _ => (0, 1),
            }
        };
        let p = dir(pel_path);
        let q = dir(pel_path + line_prog);
        if (p, q) == ((1, 0), (0, 1)) {
            return None;
        }
        let (w, h) = (width as i64, height as i64);
        // Shift so the destination coordinates start at zero.
        let off = (-(p.0.min(0) * (w - 1) + q.0.min(0) * (h - 1)), -(p.1.min(0) * (w - 1) + q.1.min(0) * (h - 1)));
        Some(Transform {
            p,
            q,
            off,
            width: (p.0.abs() * w + q.0.abs() * h) as u32,
            height: (p.1.abs() * w + q.1.abs() * h) as u32,
        })
    }

    pub fn map(&self, x: u32, y: u32) -> (u32, u32) {
        let (x, y) = (x as i64, y as i64);
        ((self.off.0 + x * self.p.0 + y * self.q.0) as u32, (self.off.1 + x * self.p.1 + y * self.q.1) as u32)
    }
}

/// TIFF Orientation tag (1–8) as CALS angles.
pub fn from_tiff(orientation: u16) -> (u32, u32) {
    match orientation {
        2 => (180, 90),
        3 => (180, 270),
        4 => (0, 90),
        5 => (270, 90),
        6 => (270, 270),
        7 => (90, 90),
        8 => (90, 270),
        _ => (0, 270),
    }
}
