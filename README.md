# Open Drawing Viewer

**[Open the viewer →](https://sinderc.github.io/DWG-Viewer/)**

An offline viewer for engineering drawings that runs entirely in the browser. Files are read
locally and never uploaded; the page's Content-Security-Policy blocks all network access. The whole
app is one self-contained HTML file, so you can save the page (File → Save Page As) and use it
without a connection.

## Formats

| Format | Notes |
| --- | --- |
| CALS raster | Type 1 |
| TIFF | Black/white and grey/colour, multi-page |
| DXF | ASCII, model space |
| DWG | R13 to 2018+ |
| HP-GL, HP-GL/2, HP RTL | Plain or wrapped in PCL/PJL |
| DWF, DWFx | Classic, DWF 6 packages and XPS pages |
| CGM | Binary and clear text, including WebCGM |

What each format draws, and what it doesn't: [docs/formats.md](docs/formats.md).

## Documentation

- [Using the viewer](docs/usage.md) — navigation, toolbar, measuring, keyboard shortcuts
- [Supported formats](docs/formats.md) — entities and features per format, known gaps
- [Development](docs/development.md) — build, test, project layout

## Quick start

```sh
cargo install wasm-bindgen-cli --version 0.2.128 --locked
npm install
npm run build        # -> dist/index.html
```

See [docs/development.md](docs/development.md) for requirements and tests.

## License

GPL-3.0 — see [LICENSE](LICENSE).
