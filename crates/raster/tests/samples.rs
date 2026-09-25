//! Decodes every file in ./samples (CALS, TIFF, DXF, DWG) (git-ignored). Run with `cargo test --release -- --ignored --nocapture`.

use std::time::Instant;

use raster::render::Raster;
use raster::rgba::ColorRaster;
use raster::tiff::Image;

/// Decodes page `page` of a sample file into an image plus a description.
fn decode(data: &[u8], is_tiff: bool, page: u32) -> Result<(Image, String), String> {
    if is_tiff {
        let (image, dpi) = raster::tiff::decode(data, page)?;
        Ok((image, format!("page {} {dpi} dpi", page + 1)))
    } else {
        let (h, bmp) = raster::cals::decode(data)?;
        Ok((Image::Bilevel(bmp), format!("{}×{} {} dpi, orient {:?}", h.width, h.height, h.dpi, h.orient)))
    }
}

#[test]
#[ignore]
fn decode_samples() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples");
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("samples/ directory").flatten().map(|e| e.path()).collect();
    entries.sort();
    let mut failures = 0;
    for path in entries.iter().filter(|p| p.is_file()) {
        let data = std::fs::read(path).unwrap();
        let vector = |ext: &str| path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
        if vector("dxf") || vector("dwg") {
            let start = Instant::now();
            match if vector("dwg") { raster::dwg::decode(&data) } else { raster::dxf::decode(&data) } {
                Ok(d) => println!(
                    "{}: {:.6} × {:.6} {}, {} paths, {} arcs, {} texts, decode {:?}",
                    path.display(), d.width, d.height, d.units, d.paths.len(), d.arcs.len(), d.texts.len(), start.elapsed()
                ),
                Err(e) => {
                    failures += 1;
                    println!("{}: ERROR {e}", path.display());
                }
            }
            continue;
        }
        let is_tiff = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
        let pages = if is_tiff { raster::tiff::page_count(&data) } else { Ok(1) };
        let result = pages.and_then(|pages| {
            (0..pages).try_for_each(|page| {
                let start = Instant::now();
                let (image, info) = decode(&data, is_tiff, page)?;
                let decoded = start.elapsed();
                let (w, h) = (2560, 1520);
                let mut out = vec![0u32; w * h];
                let (size, render): ((u32, u32), Box<dyn Fn(f64, &mut [u32])>) = match image {
                    Image::Bilevel(bmp) => {
                        let r = Raster::new(bmp);
                        ((r.bitmap.width, r.bitmap.height), Box::new(move |s, out| r.render(s, 0.0, 0.0, false, out, w)))
                    }
                    Image::Color(img) => {
                        let r = ColorRaster::new(img);
                        ((r.image().width, r.image().height), Box::new(move |s, out| r.render(s, 0.0, 0.0, false, out, w)))
                    }
                };
                // Time a Retina-sized frame at fit-to-window and a range of zoom levels.
                let fit = (w as f64 / size.0 as f64).min(h as f64 / size.1 as f64);
                let timings: Vec<String> = [fit / 4.0, fit, 0.2, 0.3, 0.5, 0.9, 2.0]
                    .iter()
                    .map(|&scale| {
                        let start = Instant::now();
                        render(scale, &mut out);
                        format!("{:.0}%: {:.1}ms", scale * 100.0, start.elapsed().as_secs_f64() * 1000.0)
                    })
                    .collect();
                println!(
                    "{}: {info} → {}×{}, decode {decoded:?}\n    render {w}×{h}: {}",
                    path.display(), size.0, size.1, timings.join(", ")
                );
                Ok(())
            })
        });
        if let Err(e) = result {
            failures += 1;
            println!("{}: ERROR {e}", path.display());
        }
    }
    assert_eq!(failures, 0);
}
