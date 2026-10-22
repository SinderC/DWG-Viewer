//! Built-in shapes for complex linetypes that refer to the shape file `ltypeshp.shx`, which
//! drawings name but do not contain.

use std::f64::consts::PI;

use super::P;

/// Strokes of shape `number` of shape file `file`, in shape units from the insertion point at the
/// origin; None unless the file is `ltypeshp.shx`, the only one built in.
pub(crate) fn strokes(file: &str, number: i64) -> Option<Vec<Vec<P>>> {
    let name = file.trim().rsplit(['/', '\\']).next()?.to_lowercase();
    if name != "ltypeshp" && name != "ltypeshp.shx" {
        return None;
    }
    ltypeshp(number)
}

fn ltypeshp(number: i64) -> Option<Vec<Vec<P>>> {
    Some(match number {
        130 => vec![vec![[0.0, 0.0], [0.0, 1.0]], vec![[0.0, 0.0], [0.0, -1.0]]],
        131 => vec![vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]],
        132 => vec![vec![[0.0, 0.0], [0.0, 1.0], [2.0, 1.0], [2.0, -1.0], [0.0, -1.0], [0.0, 0.0]]],
        133 => vec![arc([1.0, 0.0], 1.0, PI, -2.0 * PI)],
        134 => {
            let mut s = vec![[0.0, 0.0]];
            s.extend(arc([1.0, 2.0], 2.0, PI, -PI));
            s.push([2.0, 0.0]);
            vec![s]
        }
        135 => vec![vec![[0.0, 0.0], [108.0, 0.0], [123.0, 56.0], [153.0, -56.0], [168.0, 0.0], [275.0, 0.0]]],
        _ => return None,
    })
}

/// Shapes repeat thousands of times along a line and are tiny at most zooms, so arcs turn 15° per
/// segment: coarse only for a shape drawn hundreds of pixels across.
fn arc(centre: P, r: f64, start: f64, sweep: f64) -> Vec<P> {
    let steps = (sweep.abs() / (PI / 12.0)).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|i| {
            let (s, c) = (start + sweep * i as f64 / steps as f64).sin_cos();
            [centre[0] + r * c, centre[1] + r * s]
        })
        .collect()
}
