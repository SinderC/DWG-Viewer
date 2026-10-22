//! Edges of ACIS bodies (REGION, 3DSOLID, BODY), from SAT text or SAB binary data.

use std::f64::consts::TAU;

use acadrust::entities::acis::{SabReader, SatDocument, SatEdge, SatEllipseCurve, SatIntCurve, SatPoint, SatPointer, SatVertex, Sense};

use super::{P, SPLINE_STEPS};

type P3 = [f64; 3];

/// Points per full turn of an elliptical edge.
const ELLIPSE_STEPS: usize = 64;

/// Reads SAB data, or SAT text if there is none.
pub(crate) fn read(sat: &str, sab: &[u8]) -> Option<SatDocument> {
    if sab.is_empty() { SatDocument::parse(sat).ok() } else { SabReader::read(sab).ok() }
}

/// Each edge of `doc` as a polyline seen from above (Z dropped): a region's outline, a solid's
/// wireframe. Faces with no edges, such as a sphere's, draw nothing.
pub(crate) fn edges(doc: &SatDocument) -> Vec<Vec<P>> {
    let point = |v: SatPointer| -> Option<P3> {
        let vertex = SatVertex::from_record(doc.resolve(v)?)?;
        let (x, y, z) = SatPoint::from_record(doc.resolve(vertex.point())?)?.position();
        Some([x, y, z])
    };
    // The body's placement: world = s·(p·M) + T.
    let (m, t, s) = doc.placement();
    let place = |p: P3| -> P { [0, 1].map(|j| s * (p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j]) + t[j]) };
    doc.edges()
        .iter()
        .filter_map(|e| Some(edge(doc, e, point(e.start_vertex())?, point(e.end_vertex())?).into_iter().map(place).collect()))
        .collect()
}

fn edge(doc: &SatDocument, e: &SatEdge, a: P3, b: P3) -> Vec<P3> {
    let Some(curve) = doc.resolve(e.curve()) else { return vec![a, b] };
    let reversed = e.sense() == Sense::Reversed;
    if let Some(c) = SatEllipseCurve::from_record(curve) {
        let ((cx, cy, cz), (nx, ny, nz), (ux, uy, uz)) = (c.center(), c.normal(), c.major_axis());
        let (centre, u) = ([cx, cy, cz], [ux, uy, uz]);
        // Minor axis: unit normal × major, scaled by the ratio.
        let n = (nx * nx + ny * ny + nz * nz).sqrt().max(f64::MIN_POSITIVE);
        let v = [ny * uz - nz * uy, nz * ux - nx * uz, nx * uy - ny * ux].map(|x| x / n * c.ratio());
        // A vertex's angle on the ellipse; the edge runs along the curve, or against it if reversed.
        let angle = |p: P3| {
            let d = [p[0] - cx, p[1] - cy, p[2] - cz];
            (dot3(d, v) / dot3(v, v)).atan2(dot3(d, u) / dot3(u, u))
        };
        let (t0, t1) = (angle(a), angle(b));
        let mut sweep = if reversed { t0 - t1 } else { t1 - t0 }.rem_euclid(TAU);
        if sweep < 1e-9 {
            sweep = TAU;
        }
        let sweep = if reversed { -sweep } else { sweep };
        let steps = ((sweep.abs() / TAU * ELLIPSE_STEPS as f64).ceil() as usize).max(2);
        return (0..=steps)
            .map(|i| {
                let (s, c) = (t0 + sweep * i as f64 / steps as f64).sin_cos();
                [0, 1, 2].map(|k| centre[k] + u[k] * c + v[k] * s)
            })
            .collect();
    }
    if let Some(c) = SatIntCurve::from_record(curve) {
        // A reversed edge's parameters run along the reversed curve.
        let (t0, t1) = if reversed { (-e.end_param(), -e.start_param()) } else { (e.start_param(), e.end_param()) };
        let pts: Vec<P3> = c.sample_range(t0, t1, 4 * SPLINE_STEPS).into_iter().map(|(x, y, z)| [x, y, z]).collect();
        if pts.len() >= 2 {
            return pts;
        }
    }
    // Straight edges, and curves that cannot be evaluated, run between their vertices.
    vec![a, b]
}

fn dot3(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
