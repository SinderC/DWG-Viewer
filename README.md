# Drawing Viewer

Offline drawing viewer that runs entirely in the browser. Files are read locally and never uploaded;
the built page's Content-Security-Policy blocks all network access. Supported formats:

- **CALS raster** Type 1
- **TIFF**: black/white (CCITT G4, PackBits, uncompressed) and grey/colour (LZW, Deflate, JPEG,
  PackBits, uncompressed), including multi-page files. Colour images are limited to 200 megapixels.
- **DXF** (ASCII, model space): lines, polylines with arcs, circles, arcs, ellipses, splines, points,
  SOLID/TRACE/3DFACE outlines, TEXT/MTEXT/attributes, nested blocks and dimensions, in layer and
  entity colours. Not drawn: hatches, linetypes (dashes), line weights, images, 3D meshes, binary DXF.
- **DWG** R13 to 2018+ (read with the `acadrust` crate): the same model-space entities as DXF.
  R12 and older DWG files are not supported.
- **HP-GL, HP-GL/2 and HP RTL** plot files (`.plt`, `.hpgl`, …), plain or wrapped in PCL/PJL: lines,
  encoded polylines, arcs, circles, rectangles, wedges, Béziers, polygon fills (solid or shaded),
  labels, pen colours and widths, and multiple pages (`PG`). RTL raster images (1-bit, 8-bit
  indexed or 24-bit, compression 0–3) are placed from the page's top-left corner. Not drawn: line
  types, hatch patterns (filled solid), clip windows, planar RTL colour. Fills are drawn under all lines.
- **DWF**: classic single-stream files (up to 5.5, including zlib-compressed sections) and DWF 6
  packages (one page per ePlot sheet): lines, polylines, polygons, triangle strips, contour sets,
  circles, arcs, ellipses, text, colours and colour maps, line weights, layers, embedded images
  (indexed, mapped, RGB/RGBA, JPEG, PNG, Group 4), and the paper scale from the sheet descriptor or
  `PlotInfo`. Not drawn: bitonal and Group 3X images, raster overlays, line patterns, markers, 3D (W3D)
  sections; Gouraud shading is drawn flat. **DWFx** (XPS pages): paths with solid fills and strokes,
  curves and arcs, text (in the viewer's font), PNG/JPEG/TIFF image fills, transforms and resource
  dictionaries. Not drawn: gradients (drawn in their first colour), visual brushes, clipping, opacity.
  Tested only on synthetic files.
- **CGM** (binary and clear-text encodings, including WebCGM): one page per picture; polylines, polygons and polygon
  sets, rectangles, circles, ellipses and their arcs (open, pie and chord), Béziers, markers, text
  (all four text paths; restricted text shrunk to its box), colour tables and direct colour (RGB, CMYK),
  line and edge widths and types, solid and hatch fills, cell arrays (rotated or skewed too),
  Group 4/JPEG/PNG tiles and WebCGM layers. Not drawn: pattern fills (drawn solid), NURBS and conics,
  clipping, character spacing on horizontal text. Nominal marker size, hatch spacing and dash lengths
  are device-dependent in the standard and chosen here. Clear-text tiles and layers are tested only on
  synthetic input.

**Live app:** https://sinderc.github.io/DWG-Viewer/ (deployed from `main` by GitHub Actions).
To use it offline, save the page (File → Save Page As) — it is a single self-contained HTML file.

- **Zoom:** mouse wheel (around the cursor) or the − / + buttons
- **Pan:** drag with the left mouse button
- **Fit:** double-click or the Fit button; **1:1** shows one drawing pixel per screen pixel (raster only)
- **Invert:** swaps black and white, for files written with the wrong polarity; vector files switch to a black background
- **Pages:** ‹ / › step through multi-page TIFFs, HP-GL plots, DWF sheets and CGM pictures
- **Info:** file properties — size, resolution, compression and tags (TIFF), header records (CALS),
  version, units, layers and entities not drawn (DXF/DWG), language, plot size, pens and raster images (HP-GL),
  version, sheet, paper size and metadata (DWF), version, description, fonts, picture and scale (CGM)
- **Layers:** show or hide DXF/DWG/DWF/WebCGM layers or HP-GL pens, with a name filter and All on / All off; Alt-click shows
  only that layer. Layers that are off or frozen in the file start hidden.

## Build

Requires Rust via rustup (`rust-toolchain.toml` pins the version and the `wasm32-unknown-unknown`
target; rustup installs them on first build), `wasm-bindgen-cli` matching the `wasm-bindgen` crate
version, and Node.js.

```sh
cargo install wasm-bindgen-cli --version 0.2.128 --locked
npm install
npm run build        # -> dist/index.html (single self-contained file)
npm run dev          # dev server with the same app
```

If Homebrew's `rustc` shadows rustup's, put `~/.cargo/bin` first in `PATH`.

## Test

```sh
cargo test                                                   # unit tests
cargo test --release -p raster --test samples -- --ignored --nocapture   # decode everything in ./samples
cargo run --release --example make_sample -- samples/test.cal 18000 12700 090,270   # synthetic test file
cargo run --release --example dxf_to_dwg -- samples/in.dxf samples/out.dwg AC1018    # DWG from a DXF (acadrust)
cargo run --release --example make_dwf -- samples                                   # synthetic DWF (classic, 6.0) and DWFx files
cargo run --release --example make_cgm -- samples                                   # synthetic binary CGM files
```

## Layout

- `crates/raster` — Rust/WASM: CALS parsing, G4 decoding (`fax` crate), TIFF decoding (`tiff` crate),
  orientation, viewport rendering (1-bit coverage in `render.rs`, colour mip pyramid in `rgba.rs`),
  DXF parsing and block expansion into paths, exact arcs and text (`dxf/`), DWG entities mapped onto
  the same output via `acadrust` (`dwg.rs`), HP-GL/2 interpreter plus PJL/PCL/RTL unwrapping onto the same
  output (`hpgl/`), DWF W2D interpreter, ZIP/package reading and DWFx (XPS) pages onto the same output (`dwf/`), binary CGM
  interpreter, with clear text transcoded to binary, onto the same output (`cgm/`)
- `web/src/formats` — format plugin interface (`types.ts`), shared WASM adapter (`wasm.ts`), CALS, TIFF,
  DXF, DWG, HP-GL, DWF and CGM plugins (`dxf.ts` draws all vector geometry with Canvas 2D)
- `web/src/viewer.ts` — canvas view, mouse zoom/pan
