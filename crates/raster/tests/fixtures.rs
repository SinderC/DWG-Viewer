//! Decodes the committed test drawing (`fixtures/baseline.*`, written by `examples/make_baseline.rs`)
//! and checks what it is built to show. The totals catch any change in what is drawn; when a change
//! is intended, regenerate the fixtures or update the totals here.

use raster::dxf::Drawing;

fn decode(name: &str) -> Drawing {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let decoded = if name.ends_with(".dwg") { raster::dwg::decode(&data) } else { raster::dxf::decode(&data) };
    decoded.unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Paths, arcs, texts, fills and layers.
fn totals(d: &Drawing) -> [usize; 5] {
    [d.paths.len(), d.arcs.len(), d.texts.len(), d.fills.len(), d.layers.len()]
}

fn layer(d: &Drawing, name: &str) -> u32 {
    d.layers.iter().position(|l| l.name == name).unwrap_or_else(|| panic!("no layer {name}")) as u32
}

/// Transparency of each fill on layer `name`, 0 (opaque) to 63.
fn fill_transparency(d: &Drawing, name: &str) -> Vec<u32> {
    let l = layer(d, name);
    d.fills.iter().filter(|f| f.layer == l).map(|f| f.color >> 26).collect()
}

fn check(d: &Drawing) {
    assert_eq!((d.width, d.height), (1190.0, 865.0));
    let hidden: Vec<_> = d.layers.iter().filter(|l| !l.visible).map(|l| l.name.as_str()).collect();
    assert_eq!(hidden, ["X_FROZEN", "X_OFF"]);
    assert!(!d.texts.iter().any(|t| t.text.contains("PAPER SPACE")), "paper space is drawn");
    for s in ["Ελληνικά αβγ", "Русский текст", "中文 汉字", "日本語 かな", "√ ∑ ° ± µ Ø", "Leader note", "Two lines"] {
        assert!(d.texts.iter().any(|t| t.text == s), "missing text {s}");
    }

    // 50 % from the layer, 80 % from the insert (in 1/63 steps); the grid's 0, 50 and 80 % hatches.
    assert_eq!(fill_transparency(d, "X_HALF_TRANSPARENT"), [32]);
    assert_eq!(fill_transparency(d, "X_TRANSPARENCY_ByLayer___ByBlock"), [51]);
    assert_eq!(fill_transparency(d, "X_TRANSPARENCY"), [0, 32, 51]);

    // Colour and linetype: the layer's red dashes, and a block's BYBLOCK circle in the insert's blue long-dash dot.
    let red = layer(d, "X_RED_DASHED");
    assert!(d.paths.iter().filter(|p| p.layer == red).all(|p| p.color == 0xFF0000 && p.dash != 0));
    assert!(d.arcs.iter().filter(|a| a.layer == red).all(|a| a.color == 0xFF0000 && a.dash != 0));
    let inherit = layer(d, "X_COLOR___LINETYPE_inheritance");
    let by_block: Vec<_> = d.arcs.iter().filter(|a| a.layer == inherit).map(|a| (a.color, d.dashes[a.dash as usize].len())).collect();
    assert_eq!(by_block, [(0x0000FF, 4)]);

    // Wide polylines are filled bands; regions are their edges (a star and an L).
    let widths = layer(d, "X_LWPOLYLINE_widths");
    assert!(!d.paths.iter().any(|p| p.layer == widths));
    assert_eq!(d.fills.iter().filter(|f| f.layer == widths).count(), 6);
    let region = layer(d, "X_REGION");
    assert_eq!(d.paths.iter().filter(|p| p.layer == region).count(), 16);
}

#[test]
fn baseline_dxf() {
    let d = decode("baseline.dxf");
    check(&d);
    assert_eq!(totals(&d), [1672, 40, 155, 43, 60]);
}

#[test]
fn baseline_dwg() {
    let d = decode("baseline.dwg");
    check(&d);
    // Against the DXF: the curved-edge hatch and MESH come out as different path counts, and the
    // DXF's tolerance frame has one row less (acadrust writes its ^J row break as a literal).
    assert_eq!(totals(&d), [1677, 40, 156, 43, 60]);
}
