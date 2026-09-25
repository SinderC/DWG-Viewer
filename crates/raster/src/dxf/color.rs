//! AutoCAD Color Index (ACI) palette.

/// The drawing's foreground colour (ACI 7): black on white paper, white on black.
pub const FOREGROUND: u32 = 0x0100_0000;

/// ACI 1–255 as 0xRRGGBB, or `FOREGROUND` for 7. Out-of-range indices are treated as 7.
pub fn aci(index: i64) -> u32 {
    const BASIC: [u32; 9] = [0xFF0000, 0xFFFF00, 0x00FF00, 0x00FFFF, 0x0000FF, 0xFF00FF, FOREGROUND, 0x808080, 0xC0C0C0];
    const GREYS: [u32; 6] = [0x333333, 0x5B5B5B, 0x848484, 0xADADAD, 0xD6D6D6, 0xFFFFFF];
    match index {
        1..=9 => BASIC[index as usize - 1],
        // 24 hues in 15° steps; within each group of ten, pairs of decreasing value at full and half saturation.
        10..=249 => {
            let hue = (index / 10 - 1) as f64 * 15.0;
            let sub = index % 10;
            let value = [1.0, 0.65, 0.5, 0.3, 0.15][sub as usize / 2];
            let saturation = if sub % 2 == 0 { 1.0 } else { 0.5 };
            hsv(hue, saturation, value)
        }
        250..=255 => GREYS[index as usize - 250],
        _ => FOREGROUND,
    }
}

fn hsv(hue: f64, s: f64, v: f64) -> u32 {
    let f = |n: f64| {
        let k = (n + hue / 60.0) % 6.0;
        let c = v - v * s * k.min(4.0 - k).clamp(0.0, 1.0);
        (c * 255.0).floor() as u32
    };
    f(5.0) << 16 | f(3.0) << 8 | f(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette() {
        assert_eq!(aci(1), 0xFF0000);
        assert_eq!(aci(7), FOREGROUND);
        assert_eq!(aci(10), 0xFF0000);
        assert_eq!(aci(11), 0xFF7F7F);
        assert_eq!(aci(12), 0xA50000);
        assert_eq!(aci(13), 0xA55252);
        assert_eq!(aci(30), 0xFF7F00);
        assert_eq!(aci(140), 0x00BFFF);
        assert_eq!(aci(250), 0x333333);
    }
}
