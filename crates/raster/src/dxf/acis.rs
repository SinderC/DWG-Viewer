//! Edges of ACIS bodies (REGION), from SAT text or SAB binary data.

use std::f64::consts::TAU;

use acadrust::entities::acis::{SabReader, SatDocument, SatEdge, SatEllipseCurve, SatIntCurve, SatPoint, SatPointer, SatVertex, Sense};

use super::{dot, P, SPLINE_STEPS};

/// Points per full turn of an elliptical edge.
const ELLIPSE_STEPS: usize = 64;

/// Reads SAB data, or SAT text if there is none.
pub(crate) fn read(sat: &str, sab: &[u8]) -> Option<SatDocument> {
    if sab.is_empty() { SatDocument::parse(sat).ok() } else { SabReader::read(sab).ok() }
}

/// Each edge of `doc` as a polyline in the XY plane, as AutoCAD draws a region's outline.
pub(crate) fn edges(doc: &SatDocument) -> Vec<Vec<P>> {
    let point = |v: SatPointer| -> Option<P> {
        let vertex = SatVertex::from_record(doc.resolve(v)?)?;
        let (x, y, _) = SatPoint::from_record(doc.resolve(vertex.point())?)?.position();
        Some([x, y])
    };
    doc.edges().iter().filter_map(|e| Some(edge(doc, e, point(e.start_vertex())?, point(e.end_vertex())?))).collect()
}

fn edge(doc: &SatDocument, e: &SatEdge, a: P, b: P) -> Vec<P> {
    let Some(curve) = doc.resolve(e.curve()) else { return vec![a, b] };
    let reversed = e.sense() == Sense::Reversed;
    if let Some(c) = SatEllipseCurve::from_record(curve) {
        let ((cx, cy, _), (nx, ny, nz), (ux, uy, uz)) = (c.center(), c.normal(), c.major_axis());
        // Minor axis: normal × major, scaled by the ratio; only its XY part is drawn.
        let ratio = c.ratio();
        let u = [ux, uy];
        let v = [(ny * uz - nz * uy) * ratio, (nz * ux - nx * uz) * ratio];
        // A vertex's angle on the ellipse; the edge runs along the curve, or against it if reversed.
        let angle = |p: P| {
            let d = [p[0] - cx, p[1] - cy];
            (dot(d, v) / dot(v, v)).atan2(dot(d, u) / dot(u, u))
        };
        let (t0, t1) = (angle(a), angle(b));
        let mut sweep = if reversed { t0 - t1 } else { t1 - t0 }.rem_euclid(TAU);
        if sweep < 1e-9 {
            sweep = TAU;
        }
        let sweep = if reversed { -sweep } else { sweep };
        let n = ((sweep.abs() / TAU * ELLIPSE_STEPS as f64).ceil() as usize).max(2);
        return (0..=n)
            .map(|i| {
                let (s, c) = (t0 + sweep * i as f64 / n as f64).sin_cos();
                [cx + u[0] * c + v[0] * s, cy + u[1] * c + v[1] * s]
            })
            .collect();
    }
    if let Some(c) = SatIntCurve::from_record(curve) {
        // A reversed edge's parameters run along the reversed curve.
        let (t0, t1) = if reversed { (-e.end_param(), -e.start_param()) } else { (e.start_param(), e.end_param()) };
        let pts: Vec<P> = c.sample_range(t0, t1, 4 * SPLINE_STEPS).into_iter().map(|(x, y, _)| [x, y]).collect();
        if pts.len() >= 2 {
            return pts;
        }
    }
    // Straight edges, and curves that cannot be evaluated, run between their vertices.
    vec![a, b]
}
