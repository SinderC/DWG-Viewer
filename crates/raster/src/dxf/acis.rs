//! Edges of ACIS bodies (REGION, 3DSOLID, BODY), from SAT text or SAB binary data.

use std::f64::consts::TAU;

use acadrust::entities::acis::{SabReader, SatDocument, SatEdge, SatEllipseCurve, SatIntCurve, SatPoint, SatPointer, SatRecord, SatSphereSurface, SatTorusSurface, SatVertex, Sense};

use super::{P, SPLINE_STEPS};

type P3 = [f64; 3];

/// Points per full turn of an elliptical edge.
const ELLIPSE_STEPS: usize = 64;

/// Reads SAB data, or SAT text if there is none.
pub(crate) fn read(sat: &str, sab: &[u8]) -> Option<SatDocument> {
    if sab.is_empty() { SatDocument::parse(sat).ok() } else { SabReader::read(sab).ok() }
}

/// Each edge of `doc` as a polyline seen from above (Z dropped): a region's outline, a solid's
/// wireframe. Spheres and tori, whose faces have no edges, get isolines as AutoCAD draws them:
/// latitudes every 22.5° and four meridians on a sphere; tube parallels and sections every 45° on a torus.
pub(crate) fn edges(doc: &SatDocument) -> Vec<Vec<P>> {
    let point = |v: SatPointer| -> Option<P3> {
        let vertex = SatVertex::from_record(doc.resolve(v)?)?;
        let (x, y, z) = SatPoint::from_record(doc.resolve(vertex.point())?)?.position();
        Some([x, y, z])
    };
    // The body's placement: world = s·(p·M) + T.
    let (m, t, s) = doc.placement();
    let place = |p: P3| -> P { [0, 1].map(|j| s * (p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j]) + t[j]) };
    let mut out: Vec<Vec<P>> = doc
        .edges()
        .iter()
        .filter_map(|e| Some(edge(doc, e, point(e.start_vertex())?, point(e.end_vertex())?).into_iter().map(place).collect()))
        .collect();
    for face in doc.faces().iter().filter(|f| f.first_loop().is_null()) {
        let Some(surface) = doc.resolve(face.surface()) else { continue };
        out.extend(isolines(surface).into_iter().map(|c| c.into_iter().map(place).collect()));
    }
    out
}

/// Isolines of a sphere or torus surface, as closed 3D polylines.
fn isolines(surface: &SatRecord) -> Vec<Vec<P3>> {
    let v = |(x, y, z): (f64, f64, f64)| [x, y, z];
    let unit = |a: P3| {
        let n = dot3(a, a).sqrt().max(f64::MIN_POSITIVE);
        a.map(|x| x / n)
    };
    let cross = |a: P3, b: P3| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let add = |a: P3, b: P3, k: f64| [0, 1, 2].map(|i| a[i] + b[i] * k);
    if let Some(s) = SatSphereSurface::from_record(surface) {
        let (c, r, pole) = (v(s.center()), s.radius(), unit(v(s.pole())));
        let u = unit(v(s.u_direction()));
        let w = cross(pole, u);
        let mut out: Vec<_> = (-3..=3).map(|k| {
            let (sin, cos) = (k as f64 * 22.5f64).to_radians().sin_cos();
            circle(add(c, pole, r * sin), u.map(|x| x * r * cos), w.map(|x| x * r * cos))
        }).collect();
        out.extend((0..4).map(|k| {
            let (sin, cos) = (k as f64 * 45f64).to_radians().sin_cos();
            circle(c, add(u.map(|x| x * cos), w, sin).map(|x| x * r), pole.map(|x| x * r))
        }));
        return out;
    }
    if let Some(t) = SatTorusSurface::from_record(surface) {
        let (c, n, big, small) = (v(t.center()), unit(v(t.normal())), t.major_radius(), t.minor_radius());
        let u = unit(v(t.u_direction()));
        let w = cross(n, u);
        let eighths = (0..8).map(|k| (k as f64 * 45f64).to_radians().sin_cos());
        let mut out: Vec<_> = eighths.clone().map(|(sin, cos)| {
            let r = big + small * cos;
            circle(add(c, n, small * sin), u.map(|x| x * r), w.map(|x| x * r))
        }).collect();
        out.extend(eighths.map(|(sin, cos)| {
            let d = add(u.map(|x| x * cos), w, sin);
            circle(add(c, d, big), d.map(|x| x * small), n.map(|x| x * small))
        }));
        return out;
    }
    Vec::new()
}

/// The closed curve `centre + a·cos t + b·sin t`.
fn circle(centre: P3, a: P3, b: P3) -> Vec<P3> {
    (0..=ELLIPSE_STEPS)
        .map(|i| {
            let (s, c) = (TAU * i as f64 / ELLIPSE_STEPS as f64).sin_cos();
            [0, 1, 2].map(|k| centre[k] + a[k] * c + b[k] * s)
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use acadrust::entities::acis::primitives;

    /// Top-view radius of each isoline's first point, rounded to 0.1.
    fn radii(doc: &SatDocument) -> Vec<f64> {
        let mut r: Vec<f64> = edges(doc).iter().map(|l| (l[0][0].hypot(l[0][1]) * 10.0).round() / 10.0).collect();
        r.sort_by(f64::total_cmp);
        r.dedup();
        r
    }

    #[test]
    fn sphere_and_torus_isolines_match_autocad() {
        let sphere = SatDocument::parse(&primitives::build_sphere([0.0; 3], 10.0).to_sat_string()).unwrap();
        assert_eq!(edges(&sphere).len(), 11);
        // Latitudes at 0, ±22.5, ±45, ±67.5°; meridians start on the equator.
        assert_eq!(radii(&sphere), [3.8, 7.1, 9.2, 10.0]);
        let torus = SatDocument::parse(&primitives::build_torus([0.0; 3], 30.0, 8.0).to_sat_string()).unwrap();
        assert_eq!(edges(&torus).len(), 16);
        // Tube parallels at R + r·cos(45°·k); tube sections start on the outer rim.
        assert_eq!(radii(&torus), [22.0, 24.3, 30.0, 35.7, 38.0]);
    }
}
