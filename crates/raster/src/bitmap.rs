//! 1-bit bitmap, rows packed into u64 words. Bit `x % 64` of word `x / 64` is pixel x; 1 = black.

use crate::orient::Transform;

/// Upper bound on 1-bit image size (a quarter of a gigabyte of bitmap).
pub const MAX_BILEVEL_PIXELS: u64 = 2_000_000_000;

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub words_per_row: usize,
    pub data: Vec<u64>,
}

impl Bitmap {
    pub fn new(width: u32, height: u32) -> Self {
        let words_per_row = (width as usize).div_ceil(64);
        Bitmap { width, height, words_per_row, data: vec![0; words_per_row * height as usize] }
    }

    pub fn row(&self, y: u32) -> &[u64] {
        let start = y as usize * self.words_per_row;
        &self.data[start..start + self.words_per_row]
    }

    pub fn row_mut(&mut self, y: u32) -> &mut [u64] {
        let start = y as usize * self.words_per_row;
        &mut self.data[start..start + self.words_per_row]
    }

    pub fn get(&self, x: u32, y: u32) -> bool {
        self.row(y)[x as usize / 64] >> (x % 64) & 1 == 1
    }

    pub fn set(&mut self, x: u32, y: u32) {
        self.data[y as usize * self.words_per_row + x as usize / 64] |= 1 << (x % 64);
    }

    /// Fills row `y` from G4 color-change positions (the first run is white).
    pub fn set_row_from_transitions(&mut self, y: u32, transitions: &[u16]) {
        if y >= self.height {
            return;
        }
        let start = y as usize * self.words_per_row;
        let row = &mut self.data[start..start + self.words_per_row];
        for run in transitions.chunks(2) {
            let x0 = run[0] as usize;
            let x1 = run.get(1).map_or(self.width as usize, |&x| x as usize).min(self.width as usize);
            fill_bits(row, x0, x1);
        }
    }

    /// Number of black pixels in row `y`, columns `x0..x1`.
    pub fn count(&self, y: u32, x0: usize, x1: usize) -> u32 {
        if x0 >= x1 {
            return 0;
        }
        let row = self.row(y);
        let (w0, w1) = (x0 / 64, (x1 - 1) / 64);
        let first_mask = !0u64 << (x0 % 64);
        let last_mask = !0u64 >> (63 - (x1 - 1) % 64);
        if w0 == w1 {
            return (row[w0] & first_mask & last_mask).count_ones();
        }
        let middle: u32 = row[w0 + 1..w1].iter().map(|w| w.count_ones()).sum();
        (row[w0] & first_mask).count_ones() + middle + (row[w1] & last_mask).count_ones()
    }

    /// Applies CALS `rorient` angles; see [`crate::orient`].
    pub fn oriented(self, pel_path: u32, line_prog: u32) -> Bitmap {
        let Some(t) = Transform::new(self.width, self.height, pel_path, line_prog) else { return self };
        let mut out = Bitmap::new(t.width, t.height);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) {
                    let (x, y) = t.map(x, y);
                    out.set(x, y);
                }
            }
        }
        out
    }
}

fn fill_bits(row: &mut [u64], x0: usize, x1: usize) {
    for x in x0..x1.min(x0.next_multiple_of(64)) {
        row[x / 64] |= 1 << (x % 64);
    }
    let mut x = x0.next_multiple_of(64);
    while x + 64 <= x1 {
        row[x / 64] = !0;
        x += 64;
    }
    for x in x.max(x0)..x1 {
        row[x / 64] |= 1 << (x % 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_and_count() {
        let mut b = Bitmap::new(200, 1);
        b.set_row_from_transitions(0, &[3, 130, 190]);
        for x in 0..200 {
            assert_eq!(b.get(x, 0), (3..130).contains(&x) || x >= 190, "x={x}");
        }
        assert_eq!(b.count(0, 0, 200), 127 + 10);
        assert_eq!(b.count(0, 60, 70), 10);
        assert_eq!(b.count(0, 129, 191), 2);
        assert_eq!(b.count(0, 5, 5), 0);
    }

    fn l_shape() -> Bitmap {
        // 3 wide, 2 high:  X . .
        //                  X X .
        let mut b = Bitmap::new(3, 2);
        b.set(0, 0);
        b.set(0, 1);
        b.set(1, 1);
        b
    }

    fn pixels(b: &Bitmap) -> Vec<String> {
        (0..b.height).map(|y| (0..b.width).map(|x| if b.get(x, y) { 'X' } else { '.' }).collect()).collect()
    }

    #[test]
    fn orientation() {
        assert_eq!(pixels(&l_shape().oriented(0, 270)), ["X..", "XX."]);
        // Lines progress upwards: vertical flip.
        assert_eq!(pixels(&l_shape().oriented(0, 90)), ["XX.", "X.."]);
        // Pels run downwards, lines progress leftwards: the image is rotated 90° clockwise.
        assert_eq!(pixels(&l_shape().oriented(270, 270)), ["XX", "X.", ".."]);
        assert_eq!(pixels(&l_shape().oriented(180, 270)), [".XX", "..X"]);
    }
}
