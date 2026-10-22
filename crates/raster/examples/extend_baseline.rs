//! Copies a DXF or DWG and adds the grid of test cells in `shared/cells.rs` below the drawing
//! (negative Y), for side-by-side comparison with AutoCAD:
//! `cargo run --release --example extend_baseline -- samples/baseline-sample.dxf samples/baseline-extended.dxf`.
//! The output keeps the input's format (by extension) and version. The input's empty
//! "Test <Entity type>" rows are left out.

#[path = "shared/cells.rs"]
mod cells;

use acadrust::entities::LwPolyline;
use acadrust::types::{Handle, Vector3};
use acadrust::{CadDocument, DwgReader, DxfReader, EntityType};
use cells::{build, is_dwg, save, v, Sheet};

/// Removes the input's empty rows: each "Test <Entity type>" label and the frame around it.
fn drop_empty_rows(doc: &mut CadDocument) {
    let labels: Vec<(Handle, Vector3)> = doc
        .model_space_entities()
        .filter_map(|e| match e {
            EntityType::Text(t) if t.value.trim() == "Test <Entity type>" => Some((t.common.handle, t.insertion_point)),
            _ => None,
        })
        .collect();
    let around = |p: &LwPolyline, at: Vector3| {
        let (xs, ys) = (p.vertices.iter().map(|v| v.location.x), p.vertices.iter().map(|v| v.location.y));
        let (x0, x1) = (xs.clone().fold(f64::INFINITY, f64::min), xs.fold(f64::NEG_INFINITY, f64::max));
        let (y0, y1) = (ys.clone().fold(f64::INFINITY, f64::min), ys.fold(f64::NEG_INFINITY, f64::max));
        // A row, not a frame around several.
        y1 - y0 <= 100. && (x0..=x1).contains(&at.x) && (y0..=y1).contains(&at.y)
    };
    let frames: Vec<Handle> = doc
        .model_space_entities()
        .filter_map(|e| match e {
            EntityType::LwPolyline(p) if labels.iter().any(|&(_, at)| around(p, at)) => Some(p.common.handle),
            _ => None,
        })
        .collect();
    for h in labels.into_iter().map(|(h, _)| h).chain(frames) {
        doc.remove_entity(h);
        // remove_entity leaves the handle in its block record, which the writers would then save.
        doc.block_records.iter_mut().for_each(|b| b.entity_handles.retain(|&e| e != h));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output] = &args[..] else {
        panic!("usage: extend_baseline IN.(dxf|dwg) OUT.(dxf|dwg)");
    };
    let mut doc = if is_dwg(input) { DwgReader::from_file(input).unwrap().read().unwrap() } else { DxfReader::from_file(input).unwrap().read().unwrap() };
    let before = doc.entity_count();
    drop_empty_rows(&mut doc);
    // Below the model-space extents, left-aligned with them.
    let (min, _) = (doc.header.model_space_extents_min, doc.header.model_space_extents_max);
    let mut s = Sheet { top_left: v(min.x, min.y - 100.), doc, count: 0 };
    s.label("EXTRA ELEMENTS (synthetic, acadrust)", s.top_left + v(0., 15.), 12.);
    build(&mut s);
    println!("{} cells, {} → {} entities", s.count, before, s.doc.entity_count());
    save(&mut s.doc, output);
}
