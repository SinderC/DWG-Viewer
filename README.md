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

**Live app:** https://sinderc.github.io/DWG-Viewer/ (deployed from `main` by GitHub Actions).
To use it offline, save the page (File → Save Page As) — it is a single self-contained HTML file.

- **Zoom:** mouse wheel (around the cursor) or the − / + buttons
- **Pan:** drag with the left mouse button
- **Fit:** double-click or the Fit button; **1:1** shows one drawing pixel per screen pixel (raster only)
- **Invert:** swaps black and white, for files written with the wrong polarity; DXF/DWG switches to a black background
- **Pages:** ‹ / › step through multi-page TIFFs
- **Info:** file properties — size, resolution, compression and tags (TIFF), header records (CALS),
  version, units, layers and entities not drawn (DXF/DWG)
- **Layers:** show or hide DXF/DWG layers, with a name filter and All on / All off; Alt-click shows
  only that layer. Layers that are off or frozen in the file start hidden.

## Build

Requires Rust via rustup (the `wasm32-unknown-unknown` target), `wasm-bindgen-cli` matching the
`wasm-bindgen` crate version, and Node.js.

```sh
rustup target add wasm32-unknown-unknown
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
```

## Layout

- `crates/raster` — Rust/WASM: CALS parsing, G4 decoding (`fax` crate), TIFF decoding (`tiff` crate),
  orientation, viewport rendering (1-bit coverage in `render.rs`, colour mip pyramid in `rgba.rs`),
  DXF parsing and block expansion into paths, exact arcs and text (`dxf/`), DWG entities mapped onto
  the same output via `acadrust` (`dwg.rs`)
- `web/src/formats` — format plugin interface (`types.ts`), shared WASM adapter (`wasm.ts`), CALS, TIFF,
  DXF and DWG plugins (`dxf.ts` strokes the geometry with Canvas 2D)
- `web/src/viewer.ts` — canvas view, mouse zoom/pan
