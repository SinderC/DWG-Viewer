//! Grey and colour images: RGBA pixels plus a mip pyramid for zoomed-out rendering.

use crate::orient::Transform;
use crate::render::{centres, BACKGROUND};

/// Upper bound on colour image size. With the pyramid that is about 1.1 GB of WASM memory.
pub const MAX_PIXELS: u64 = 200_000_000;

/// Little-endian RGBA packed as u32 (0xAABBGGRR), fully opaque.
pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    0xFF00_0000 | (b as u32) << 16 | (g as u32) << 8 | r as u32
}

pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u32>,
}

impl RgbaImage {
    pub fn get(&self, x: u32, y: u32) -> u32 {
        self.data[y as usize * self.width as usize + x as usize]
    }

    /// Applies CALS `rorient` angles; see [`crate::orient`].
    pub fn oriented(self, pel_path: u32, line_prog: u32) -> RgbaImage {
        let Some(t) = Transform::new(self.width, self.height, pel_path, line_prog) else { return self };
        let mut data = vec![0; self.data.len()];
        for y in 0..self.height {
            for x in 0..self.width {
                let (dx, dy) = t.map(x, y);
                data[dy as usize * t.width as usize + dx as usize] = self.get(x, y);
            }
        }
        RgbaImage { width: t.width, height: t.height, data }
    }

    /// Half size, each pixel the average of the (up to) 2×2 pixels it covers.
    fn halved(&self) -> RgbaImage {
        let (w, h) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let mut data = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h {
            for x in 0..w {
                let (mut sum, mut n) = ([0u32; 3], 0);
                for sy in 2 * y..(2 * y + 2).min(self.height) {
                    for sx in 2 * x..(2 * x + 2).min(self.width) {
                        let px = self.get(sx, sy);
                        for (c, s) in sum.iter_mut().enumerate() {
                            *s += px >> (8 * c) & 0xFF;
                        }
                        n += 1;
                    }
                }
                let avg = |c: usize| ((sum[c] + n / 2) / n) as u8;
                data.push(rgb(avg(0), avg(1), avg(2)));
            }
        }
        RgbaImage { width: w, height: h, data }
    }
}

/// An image plus successively halved copies; level `k` is 1/2^k of the full size.
pub struct ColorRaster {
    levels: Vec<RgbaImage>,
}

impl ColorRaster {
    pub fn new(image: RgbaImage) -> Self {
        let mut levels = vec![image];
        loop {
            let last = levels.last().unwrap();
            if last.width == 1 && last.height == 1 {
                break;
            }
            levels.push(last.halved());
        }
        ColorRaster { levels }
    }

    pub fn image(&self) -> &RgbaImage {
        &self.levels[0]
    }

    /// Same parameters as [`crate::render::Raster::render`]. Zoomed out, samples the pyramid
    /// level with at most 2 source pixels per output pixel.
    pub fn render(&self, scale: f64, ox: f64, oy: f64, invert: bool, out: &mut [u32], out_w: usize) {
        let full = self.image();
        let level = if scale >= 0.5 { 0 } else { ((1.0 / scale).log2().floor() as usize).min(self.levels.len() - 1) };
        let img = &self.levels[level];
        let xs = centres(out_w, ox, scale, full.width);
        let ys = centres(out.len() / out_w, oy, scale, full.height);
        let flip = if invert { 0x00FF_FFFF } else { 0 };
        for (line, sy) in out.chunks_exact_mut(out_w).zip(ys) {
            let Some(sy) = sy else {
                line.fill(BACKGROUND);
                continue;
            };
            let row = &img.data[(sy >> level) as usize * img.width as usize..][..img.width as usize];
            for (px, sx) in line.iter_mut().zip(&xs) {
                *px = match sx {
                    Some(sx) => row[(sx >> level) as usize] ^ flip,
                    None => BACKGROUND,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, data: Vec<u32>) -> RgbaImage {
        RgbaImage { width: w, height: h, data }
    }

    #[test]
    fn pyramid_averages() {
        // 3×1: the odd last column is averaged on its own.
        let r = ColorRaster::new(image(3, 1, vec![rgb(0, 0, 0), rgb(200, 100, 50), rgb(10, 20, 30)]));
        assert_eq!(r.levels.len(), 3);
        assert_eq!(r.levels[1].data, [rgb(100, 50, 25), rgb(10, 20, 30)]);
        assert_eq!(r.levels[2].data, [rgb(55, 35, 28)]);
    }

    #[test]
    fn nearest_with_margin_and_invert() {
        let (a, b) = (rgb(10, 20, 30), rgb(200, 100, 0));
        let r = ColorRaster::new(image(2, 1, vec![a, b]));
        let render = |invert| {
            let mut out = vec![0; 6];
            r.render(2.0, -0.5, 0.0, invert, &mut out, 6);
            out
        };
        assert_eq!(render(false), [BACKGROUND, a, a, b, b, BACKGROUND]);
        assert_eq!(render(true), [BACKGROUND, rgb(245, 235, 225), rgb(245, 235, 225), rgb(55, 155, 255), rgb(55, 155, 255), BACKGROUND]);
    }

    #[test]
    fn zoomed_out_uses_pyramid() {
        // 4×1, rendered at 1/4 scale: one pixel, the average of all four. The image is only a
        // quarter of an output pixel high, so shift it under the pixel centre.
        let r = ColorRaster::new(image(4, 1, vec![rgb(0, 0, 0), rgb(40, 0, 0), rgb(80, 0, 0), rgb(120, 0, 0)]));
        let mut out = vec![0; 1];
        r.render(0.25, 0.0, -1.5, false, &mut out, 1);
        assert_eq!(out, [rgb(60, 0, 0)]);
    }

    #[test]
    fn orientation() {
        // 2×1 rotated 90° clockwise (TIFF orientation 6) becomes 1×2, left pixel on top.
        let (a, b) = (rgb(1, 1, 1), rgb(2, 2, 2));
        let o = image(2, 1, vec![a, b]).oriented(270, 270);
        assert_eq!((o.width, o.height, o.data), (1, 2, vec![a, b]));
    }
}
