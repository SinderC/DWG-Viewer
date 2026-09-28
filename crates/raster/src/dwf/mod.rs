//! DWF and DWFx files, drawn through the DXF output helpers.
//!
//! A classic DWF (up to 5.5) is one W2D graphics stream. A DWF 6 package is `(DWF V06.00)`
//! followed by a ZIP archive: `manifest.xml` lists the sections, and each ePlot section's
//! `descriptor.xml` names its W2D streams, paper size and the logical-to-paper transform. A DWFx
//! file is an XPS package (see `xps`).

mod image;
mod palette;
mod w2d;
mod xml;
mod xps;
mod zip;

use crate::dxf::{self, World};
use w2d::Paper;
use zip::Zip;

const W2D_MIME: &str = "application/x-w2d";

/// Decodes sheet `page` (0-based) and returns it with the sheet count.
pub fn decode(data: &[u8], page: u32) -> Result<(dxf::Drawing, u32), String> {
    if data.starts_with(b"PK\x03\x04") {
        return xps::decode(data, page);
    }
    if !data.starts_with(b"(DWF V") {
        return Err("Not a DWF file".into());
    }
    if data.get(12..16) == Some(b"PK\x03\x04") { package(data, page) } else { classic(data, page) }
}

fn no_page(page: u32) -> String {
    format!("Page {} does not exist", page + 1)
}

fn classic(data: &[u8], page: u32) -> Result<(dxf::Drawing, u32), String> {
    if page > 0 {
        return Err(no_page(page));
    }
    let mut world = World::new(Vec::new());
    let s = w2d::draw(data, &mut world, None)?;
    let mut info = vec![("DWF version".to_string(), s.version)];
    info.extend(s.info);
    world.finish(s.paper.map_or("", |p| p.units), info).map(|d| (d, 1))
}

/// A sheet of a DWF 6 package.
struct Sheet {
    title: String,
    /// W2D streams drawn in order: the sheet's graphics, then overlays.
    streams: Vec<String>,
    paper: Option<Paper>,
    /// "w × h units" for display.
    size: Option<String>,
    images: usize,
}

fn package(data: &[u8], page: u32) -> Result<(dxf::Drawing, u32), String> {
    let zip = Zip::open(data)?;
    let sheets = sheets(&zip);
    let pages = sheets.len() as u32;
    if pages == 0 {
        return Err("The DWF package has no 2D sheets".into());
    }
    let sheet = sheets.get(page as usize).ok_or_else(|| no_page(page))?;
    let mut world = World::new(Vec::new());
    let mut info = vec![("DWF version".to_string(), "6.0 package".to_string())];
    if !sheet.title.is_empty() {
        info.push(("Sheet".into(), sheet.title.clone()));
    }
    if let Some(size) = &sheet.size {
        info.push(("Paper".into(), size.clone()));
    }
    let mut paper = sheet.paper;
    for href in &sheet.streams {
        let bytes = zip.read(href).ok_or_else(|| format!("{href} is missing from the package"))??;
        let s = w2d::draw(&bytes, &mut world, sheet.paper)?;
        paper = paper.or(s.paper);
        // The descriptor's paper size wins over the stream's own.
        info.extend(s.info.into_iter().filter(|(k, _)| k != "Paper" || sheet.size.is_none()));
    }
    for _ in 0..sheet.images {
        world.skip("Raster overlay");
    }
    world.finish(paper.map_or("", |p| p.units), info).map(|d| (d, pages))
}

/// ePlot sections in manifest order; without a usable manifest, every W2D stream is a sheet.
fn sheets(zip: &Zip) -> Vec<Sheet> {
    let manifest = zip.read("manifest.xml").and_then(Result::ok).and_then(|m| xml::parse(&m).ok());
    let sections = manifest.iter().flat_map(|m| m.descendants()).filter(|n| n.name == "Section");
    let sheets: Vec<Sheet> = sections
        .filter(|s| s.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("com.autodesk.dwf.ePlot")))
        .filter_map(|s| {
            let descriptor = s.descendants().into_iter().find(|r| r.name == "Resource" && r.attr("role") == Some("descriptor"))?.attr("href")?;
            descriptor_sheet(zip, descriptor, s.attr("title").unwrap_or_default())
        })
        .collect();
    if !sheets.is_empty() {
        return sheets;
    }
    zip.names()
        .filter(|n| n.to_lowercase().ends_with(".w2d"))
        .map(|n| {
            let title = n.rsplit(['/', '\\']).next().unwrap_or(n).trim_end_matches(".w2d").to_string();
            Sheet { title, streams: vec![n.to_string()], paper: None, size: None, images: 0 }
        })
        .collect()
}

fn descriptor_sheet(zip: &Zip, href: &str, title: &str) -> Option<Sheet> {
    let doc = xml::parse(&zip.read(href)?.ok()?).ok()?;
    let nodes = doc.descendants();
    let dir = href.rfind(['/', '\\']).map_or("", |i| &href[..=i]);
    // Resource paths are package paths; accept ones relative to the descriptor too.
    let resolve = |r: &str| if zip.contains(r) { r.to_string() } else { format!("{dir}{r}") };
    let paper_node = nodes.iter().find(|n| n.name == "Paper");
    let units = match paper_node.and_then(|p| p.attr("units")) {
        Some(u) if u.eq_ignore_ascii_case("mm") => "mm",
        _ => "in",
    };
    let size = paper_node.and_then(|p| {
        let (w, h): (f64, f64) = (p.attr("width")?.parse().ok()?, p.attr("height")?.parse().ok()?);
        Some(format!("{} × {} {units}", w2d::round(w), w2d::round(h)))
    });
    let graphics: Vec<_> = nodes.iter().filter(|n| n.attr("mime").is_some_and(|m| m.eq_ignore_ascii_case(W2D_MIME))).collect();
    let scale = graphics.iter().find_map(|g| {
        let t: Vec<f64> = g.attr("transform")?.split_whitespace().map(str::parse).collect::<Result<_, _>>().ok()?;
        Some(t.first()?.hypot(*t.get(1)?)).filter(|s| s.is_finite() && *s > 0.0)
    });
    let streams: Vec<String> = graphics.iter().filter_map(|g| g.attr("href")).map(resolve).collect();
    let images = nodes.iter().filter(|n| n.attr("mime").is_some_and(|m| m.starts_with("image/")) && n.attr("role").is_some_and(|r| r.contains("raster"))).count();
    (!streams.is_empty()).then(|| Sheet { title: title.to_string(), streams, paper: scale.map(|scale| Paper { scale, units }), size, images })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(sections: &[(&str, &str)]) -> String {
        let s: String = sections
            .iter()
            .map(|(title, dir)| format!(r#"<dwf:Section type="com.autodesk.dwf.ePlot" title="{title}"><dwf:Resource role="descriptor" href="{dir}\descriptor.xml"/></dwf:Section>"#))
            .collect();
        format!(r#"<?xml version="1.0"?><dwf:Manifest xmlns:dwf="x"><dwf:Sections>{s}</dwf:Sections></dwf:Manifest>"#)
    }

    fn descriptor(dir: &str, transform: &str) -> String {
        format!(
            r#"<ePlot:Page xmlns:ePlot="y"><ePlot:Paper units="mm" width="297" height="210"/><dwf:Resources>
            <ePlot:GraphicResource role="2d streaming graphics" mime="application/x-w2d" href="{dir}\g.w2d" transform="{transform}"/>
            <ePlot:ImageResource role="raster overlay" mime="image/png" href="{dir}\i.png"/></dwf:Resources></ePlot:Page>"#
        )
    }

    fn dwf6() -> Vec<u8> {
        let m = manifest(&[("First", "s1"), ("Second", "s2")]);
        let d1 = descriptor("s1", "0.01 0 0 0 0 0.01 0 0 0 0 1 0 0 0 0 1");
        let d2 = descriptor("s2", "");
        let (g1, g2) = (w2d::tests::stream(b"L 0,0 1000,0 L 0,0 0,500 "), w2d::tests::stream(b"L 0,0 5,5 "));
        let entries: [(&str, &[u8]); 5] =
            [("manifest.xml", m.as_bytes()), ("s1\\descriptor.xml", d1.as_bytes()), ("s1\\g.w2d", &g1), ("s2\\descriptor.xml", d2.as_bytes()), ("s2\\g.w2d", &g2)];
        [b"(DWF V06.00)".as_slice(), &zip::tests::build(&entries, true)].concat()
    }

    #[test]
    fn classic_stream() {
        let (d, pages) = decode(b"(DWF V00.55)L 0,0 10,10 ", 0).unwrap();
        assert_eq!(pages, 1);
        assert_eq!(d.paths.len(), 1);
        assert!(d.info.contains(&("DWF version".into(), "0.55".into())));
        assert!(decode(b"(DWF V00.55)L 0,0 10,10 ", 1).is_err());
    }

    #[test]
    fn package_sheets() {
        let data = dwf6();
        let (d, pages) = decode(&data, 0).unwrap();
        assert_eq!(pages, 2);
        assert_eq!(d.units, "mm");
        assert!((d.width - 10.0).abs() < 1e-9 && (d.height - 5.0).abs() < 1e-9);
        assert!(d.info.contains(&("Sheet".into(), "First".into())));
        assert!(d.info.contains(&("Paper".into(), "297 × 210 mm".into())));
        assert!(d.info.contains(&("Not drawn".into(), "Raster overlay × 1".into())));
        let (d, _) = decode(&data, 1).unwrap();
        assert_eq!(d.units, "");
        assert!(d.info.contains(&("Sheet".into(), "Second".into())));
        assert!(decode(&data, 2).is_err());
    }

    #[test]
    fn package_without_manifest_uses_streams() {
        let g = w2d::tests::stream(b"L 0,0 5,5 ");
        let data = [b"(DWF V06.00)".as_slice(), &zip::tests::build(&[("a/one.w2d", &g), ("a/two.w2d", &g)], false)].concat();
        let (d, pages) = decode(&data, 1).unwrap();
        assert_eq!(pages, 2);
        assert!(d.info.contains(&("Sheet".into(), "two".into())));
    }
}
