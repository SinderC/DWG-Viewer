//! Converts a DXF to DWG with acadrust: `cargo run --release --example dxf_to_dwg -- in.dxf out.dwg [AC1032]`.
//! The DWG comes from the same library the viewer reads it with, so it tests the plumbing, not
//! compatibility with AutoCAD's files.

use acadrust::types::DxfVersion;
use acadrust::{DwgWriter, DxfReader};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut doc = DxfReader::from_file(&args[1]).unwrap().read().unwrap();
    doc.version = DxfVersion::parse(args.get(3).map_or("AC1032", String::as_str)).expect("version code such as AC1015");
    DwgWriter::write_to_file(&args[2], &doc).unwrap();
}
