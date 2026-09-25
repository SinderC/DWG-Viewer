//! Renders a viewport of a bitmap into an RGBA buffer.
//!
//! Zoomed out, each output pixel shows the share of ink among the source pixels it covers.
//! Each source row (or row of coarse cells) is visited once per frame; per-column counts
//! come from prefix sums, so the ink between two column edges is `P(right) - P(left)`.

use crate::bitmap::Bitmap;

// Little-endian RGBA packed as u32 (0xAABBGGRR).
const WHITE: u32 = 0xFFFF_FFFF;
const BLACK: u32 = 0xFF00_0000;
pub(crate) const BACKGROUND: u32 = 0xFF3A_3632;

/// Side length of the coarse coverage cells, used when zoomed out to 1/CELL or less.
const CELL: u32 = 4;

fn grey(v: u8) -> u32 {
    let v = v as u32;
    0xFF00_0000 | v << 16 | v << 8 | v
}

/// A bitmap plus a precomputed count of black pixels per CELL×CELL block.
pub struct Raster {
    pub bitmap: Bitmap,
    coarse: Vec<u8>,
    coarse_w: u32,
    coarse_h: u32,
}

impl Raster {
    pub fn new(bitmap: Bitmap) -> Self {
        let (coarse_w, coarse_h) = (bitmap.width.div_ceil(CELL), bitmap.height.div_ceil(CELL));
        let mut coarse = vec![0u8; (coarse_w * coarse_h) as usize];
        for y in 0..bitmap.height {
            let cells = &mut coarse[((y / CELL) * coarse_w) as usize..][..coarse_w as usize];
            // CELL divides 64, so each word holds 64 / CELL whole cells. Bits past the width are 0.
            for (cell_bits, c) in bitmap.row(y).iter().flat_map(|&w| (0..64).step_by(CELL as usize).map(move |s| w >> s)).zip(cells) {
                *c += (cell_bits & ((1 << CELL) - 1)).count_ones() as u8;
            }
        }
        Raster { bitmap, coarse, coarse_w, coarse_h }
    }

    /// `scale` is screen pixels per source pixel; `(ox, oy)` is the source coordinate at the
    /// top-left corner of the output. `invert` swaps black and white.
    pub fn render(&self, scale: f64, ox: f64, oy: f64, invert: bool, out: &mut [u32], out_w: usize) {
        if scale >= 1.0 {
            self.render_nearest(scale, ox, oy, invert, out, out_w);
        } else if scale > 1.0 / CELL as f64 {
            self.render_exact(scale, ox, oy, invert, out, out_w);
        } else {
            self.render_coarse(scale, ox, oy, invert, out, out_w);
        }
    }

    fn render_nearest(&self, scale: f64, ox: f64, oy: f64, invert: bool, out: &mut [u32], out_w: usize) {
        let bmp = &self.bitmap;
        let (ink, paper) = if invert { (WHITE, BLACK) } else { (BLACK, WHITE) };
        let xs = centres(out_w, ox, scale, bmp.width);
        let ys = centres(out.len() / out_w, oy, scale, bmp.height);
        for (line, sy) in out.chunks_exact_mut(out_w).zip(ys) {
            let Some(sy) = sy else {
                line.fill(BACKGROUND);
                continue;
            };
            let row = bmp.row(sy);
            for (px, sx) in line.iter_mut().zip(&xs) {
                *px = match sx {
                    Some(sx) if row[*sx as usize / 64] >> (sx % 64) & 1 == 1 => ink,
                    Some(_) => paper,
                    None => BACKGROUND,
                };
            }
        }
    }

    /// Counts individual source pixels.
    fn render_exact(&self, scale: f64, ox: f64, oy: f64, invert: bool, out: &mut [u32], out_w: usize) {
        let bmp = &self.bitmap;
        let xe = edges(out_w, ox, 1.0 / scale, bmp.width);
        let ye = edges(out.len() / out_w, oy, 1.0 / scale, bmp.height);
        // P(e) = prefix[w] + popcount(row[w] & mask), where word w holds pixel e - 1.
        let lookup: Vec<(usize, u64)> = xe
            .iter()
            .map(|&e| match e {
                0 => (0, 0),
                _ => (((e - 1) / 64) as usize, !0 >> (63 - (e - 1) % 64)),
            })
            .collect();
        let mut prefix = vec![0u32; bmp.words_per_row];
        let mut sums = vec![0u32; out_w + 1];
        for (line, y) in out.chunks_exact_mut(out_w).zip(ye.windows(2)) {
            if y[0] == y[1] {
                line.fill(BACKGROUND);
                continue;
            }
            sums.fill(0);
            for y in y[0]..y[1] {
                let row = bmp.row(y);
                let mut total = 0;
                for (p, w) in prefix.iter_mut().zip(row) {
                    *p = total;
                    total += w.count_ones();
                }
                for (s, &(w, mask)) in sums.iter_mut().zip(&lookup) {
                    *s += prefix[w] + (row[w] & mask).count_ones();
                }
            }
            shade_line(line, &sums, &xe, y[1] - y[0], invert);
        }
    }

    /// Counts whole CELL×CELL blocks; cell edges are at most 1/CELL of an output pixel off.
    fn render_coarse(&self, scale: f64, ox: f64, oy: f64, invert: bool, out: &mut [u32], out_w: usize) {
        let (cell, bmp) = (CELL as f64, &self.bitmap);
        let xe = edges(out_w, ox / cell, 1.0 / (scale * cell), self.coarse_w);
        let ye = edges(out.len() / out_w, oy / cell, 1.0 / (scale * cell), self.coarse_h);
        let to_pixels = |c: u32, limit: u32| (c * CELL).min(limit);
        let pixel_xe: Vec<u32> = xe.iter().map(|&c| to_pixels(c, bmp.width)).collect();
        // Only the visible cell columns are summed.
        let (c0, c1) = (xe[0] as usize, xe[out_w] as usize);
        let mut columns = vec![0u32; c1 - c0];
        let mut sums = vec![0u32; out_w + 1];
        for (line, y) in out.chunks_exact_mut(out_w).zip(ye.windows(2)) {
            if y[0] == y[1] {
                line.fill(BACKGROUND);
                continue;
            }
            columns.fill(0);
            for y in y[0]..y[1] {
                let start = (y * self.coarse_w) as usize;
                for (sum, &c) in columns.iter_mut().zip(&self.coarse[start + c0..start + c1]) {
                    *sum += c as u32;
                }
            }
            for (i, e) in xe.windows(2).enumerate() {
                let range = e[0] as usize - c0..e[1] as usize - c0;
                sums[i + 1] = sums[i] + columns[range].iter().sum::<u32>();
            }
            let rows = to_pixels(y[1], bmp.height) - to_pixels(y[0], bmp.height);
            shade_line(line, &sums, &pixel_xe, rows, invert);
        }
    }
}

/// Source pixel under each output pixel's centre, `None` if outside the image.
pub(crate) fn centres(n: usize, origin: f64, scale: f64, limit: u32) -> Vec<Option<u32>> {
    (0..n)
        .map(|i| {
            let s = (origin + (i as f64 + 0.5) / scale).floor();
            (s >= 0.0 && s < limit as f64).then_some(s as u32)
        })
        .collect()
}

/// Output pixel edges `0..=n` mapped to source units, clamped to `0..=limit`.
fn edges(n: usize, origin: f64, units_per_px: f64, limit: u32) -> Vec<u32> {
    (0..=n).map(|i| (origin + i as f64 * units_per_px).floor().clamp(0.0, limit as f64) as u32).collect()
}

/// Shades one output row. `sums[i]` is the ink left of column edge `i`, `xe[i]` the edge in
/// source pixels, and `rows` the number of source rows covered. Coverage is boosted with a
/// square root so thin lines stay visible when zoomed far out.
fn shade_line(line: &mut [u32], sums: &[u32], xe: &[u32], rows: u32, invert: bool) {
    for ((px, s), e) in line.iter_mut().zip(sums.windows(2)).zip(xe.windows(2)) {
        let total = (e[1] - e[0]) * rows;
        *px = if total == 0 {
            BACKGROUND
        } else {
            let ink = s[1] - s[0];
            let dark = if invert { total - ink } else { ink };
            let coverage = (dark as f32 / total as f32).sqrt();
            grey((255.5 - 255.0 * coverage) as u8)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(r: &Raster, scale: f64, ox: f64, oy: f64, invert: bool, w: usize, h: usize) -> Vec<u32> {
        let mut out = vec![0; w * h];
        r.render(scale, ox, oy, invert, &mut out, w);
        out
    }

    #[test]
    fn nearest_at_2x_with_margin() {
        let mut b = Bitmap::new(2, 1);
        b.set(1, 0);
        let r = Raster::new(b);
        // Output pixel 0 falls left of the image.
        assert_eq!(render(&r, 2.0, -0.5, 0.0, false, 6, 1), [BACKGROUND, WHITE, WHITE, BLACK, BLACK, BACKGROUND]);
        assert_eq!(render(&r, 2.0, -0.5, 0.0, true, 6, 1), [BACKGROUND, BLACK, BLACK, WHITE, WHITE, BACKGROUND]);
    }

    #[test]
    fn area_coverage() {
        // 4×4 source, left half black, rendered at 1/2 scale → 2×2 output.
        let mut b = Bitmap::new(4, 4);
        for y in 0..4 {
            b.set(0, y);
            b.set(1, y);
        }
        b.set(2, 0); // top-right 2×2 block becomes 1/4 covered
        let r = Raster::new(b);
        assert_eq!(render(&r, 0.5, 0.0, 0.0, false, 2, 2), [BLACK, grey(128), BLACK, WHITE]);
        // Inverted, the top-right block is 3/4 dark: 255 * (1 - sqrt(0.75)) = 34.
        assert_eq!(render(&r, 0.5, 0.0, 0.0, true, 2, 2), [WHITE, grey(34), WHITE, BLACK]);
    }

    /// Pseudo-random bitmap with deterministic content.
    fn noise(w: u32, h: u32) -> Bitmap {
        let mut b = Bitmap::new(w, h);
        let mut seed = 12345u64;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                if seed >> 61 == 0 {
                    b.set(x, y);
                }
            }
        }
        b
    }

    #[test]
    fn exact_matches_naive_count() {
        let r = Raster::new(noise(203, 97));
        for (scale, ox, oy) in [(0.9, 0.0, 0.0), (0.5, -7.3, 3.9), (0.3, 64.0, -20.0), (0.26, 130.5, 50.2)] {
            let (w, h) = (90, 40);
            let mut out = vec![0; w * h];
            r.render_exact(scale, ox, oy, false, &mut out, w);
            let (xe, ye) = (edges(w, ox, 1.0 / scale, 203), edges(h, oy, 1.0 / scale, 97));
            for y in 0..h {
                let ink: Vec<u32> = (0..=w)
                    .map(|e| (ye[y]..ye[y + 1]).map(|sy| (0..xe[e]).filter(|&sx| r.bitmap.get(sx, sy)).count() as u32).sum())
                    .collect();
                let mut expected = vec![0; w];
                shade_line(&mut expected, &ink, &xe, ye[y + 1] - ye[y], false);
                assert_eq!(out[y * w..][..w], expected, "scale {scale} row {y}");
            }
        }
    }

    #[test]
    fn coarse_matches_exact_on_cell_boundaries() {
        // 203×97 is not a multiple of CELL, so the last cells are clipped by the image edges.
        let r = Raster::new(noise(203, 97));
        for (scale, ox, oy) in [(0.25, 0.0, 0.0), (0.125, 8.0, -16.0), (0.0625, -32.0, 48.0)] {
            let (w, h) = (60, 30);
            let (mut coarse, mut exact) = (vec![0; w * h], vec![0; w * h]);
            r.render_coarse(scale, ox, oy, false, &mut coarse, w);
            r.render_exact(scale, ox, oy, false, &mut exact, w);
            assert_eq!(coarse, exact, "scale {scale}");
        }
    }
}
