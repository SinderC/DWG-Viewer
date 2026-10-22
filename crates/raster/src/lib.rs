//! Drawing decoding (raster, DXF, DWG, HP-GL and DWF) and raster viewport rendering, exported to JavaScript via wasm-bindgen.

pub mod bitmap;
pub mod cals;
pub mod cgm;
pub mod dwf;
pub mod dwg;
pub mod dxf;
pub mod hpgl;
pub mod orient;
pub mod render;
pub mod rgba;
pub mod tiff;

use render::Raster;
use rgba::ColorRaster;
use wasm_bindgen::prelude::*;

enum Image {
    Bilevel(Raster),
    Color(ColorRaster),
}

#[wasm_bindgen]
pub struct RasterDoc {
    image: Image,
    dpi: u32,
    info: Vec<(String, String)>,
    out: Vec<u32>,
}

/// (label, value) rows flattened to label, value, label, value, … for JavaScript.
fn flatten(rows: &[(String, String)]) -> Vec<String> {
    rows.iter().flat_map(|(k, v)| [k.clone(), v.clone()]).collect()
}

/// Number of pages in a TIFF file.
#[wasm_bindgen(js_name = tiffPageCount)]
pub fn tiff_page_count(data: &[u8]) -> Result<u32, JsError> {
    tiff::page_count(data).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen]
impl RasterDoc {
    #[wasm_bindgen(js_name = openCals)]
    pub fn open_cals(data: &[u8]) -> Result<RasterDoc, JsError> {
        let (header, bitmap) = cals::decode(data).map_err(|e| JsError::new(&e))?;
        Ok(RasterDoc { image: Image::Bilevel(Raster::new(bitmap)), dpi: header.dpi, info: cals::info(data), out: Vec::new() })
    }

    /// Opens page `page` (0-based) of a TIFF file.
    #[wasm_bindgen(js_name = openTiff)]
    pub fn open_tiff(data: &[u8], page: u32) -> Result<RasterDoc, JsError> {
        let (image, dpi) = tiff::decode(data, page).map_err(|e| JsError::new(&e))?;
        let image = match image {
            tiff::Image::Bilevel(bitmap) => Image::Bilevel(Raster::new(bitmap)),
            tiff::Image::Color(rgba) => Image::Color(ColorRaster::new(rgba)),
        };
        let info = tiff::info(data, page).map_err(|e| JsError::new(&e))?;
        Ok(RasterDoc { image, dpi, info, out: Vec::new() })
    }

    fn size(&self) -> (u32, u32) {
        match &self.image {
            Image::Bilevel(r) => (r.bitmap.width, r.bitmap.height),
            Image::Color(r) => (r.image().width, r.image().height),
        }
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.size().0
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.size().1
    }

    #[wasm_bindgen(getter)]
    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    /// Format-specific properties as label, value, label, value, …
    pub fn info(&self) -> Vec<String> {
        flatten(&self.info)
    }

    /// Renders the viewport and returns a pointer into WASM memory to `w * h` RGBA pixels.
    /// The pointer is valid until the next call.
    pub fn render(&mut self, scale: f64, ox: f64, oy: f64, invert: bool, w: u32, h: u32) -> *const u32 {
        self.out.resize(w as usize * h as usize, 0);
        match &self.image {
            Image::Bilevel(r) => r.render(scale, ox, oy, invert, &mut self.out, w as usize),
            Image::Color(r) => r.render(scale, ox, oy, invert, &mut self.out, w as usize),
        }
        self.out.as_ptr()
    }
}

/// A DXF, DWG, HP-GL, DWF or CGM drawing as flat arrays for the canvas renderer. Coordinates are in drawing
/// units, relative to the top-left of the extents with Y down. Colours are 0xRRGGBB, `FOREGROUND`
/// or `BACKGROUND`.
#[wasm_bindgen]
pub struct DxfDoc {
    drawing: dxf::Drawing,
    pages: u32,
}

#[wasm_bindgen]
impl DxfDoc {
    pub fn open(data: &[u8]) -> Result<DxfDoc, JsError> {
        Ok(DxfDoc { drawing: dxf::decode(data).map_err(|e| JsError::new(&e))?, pages: 1 })
    }

    /// Opens a DWG (R13 and later) through the same vector pipeline.
    #[wasm_bindgen(js_name = openDwg)]
    pub fn open_dwg(data: &[u8]) -> Result<DxfDoc, JsError> {
        Ok(DxfDoc { drawing: dwg::decode(data).map_err(|e| JsError::new(&e))?, pages: 1 })
    }

    /// Opens page `page` (0-based) of an HP-GL, HP-GL/2 or HP RTL plot file.
    #[wasm_bindgen(js_name = openHpgl)]
    pub fn open_hpgl(data: &[u8], page: u32) -> Result<DxfDoc, JsError> {
        let (drawing, pages) = hpgl::decode(data, page).map_err(|e| JsError::new(&e))?;
        Ok(DxfDoc { drawing, pages })
    }

    /// Opens sheet `page` (0-based) of a DWF file.
    #[wasm_bindgen(js_name = openDwf)]
    pub fn open_dwf(data: &[u8], page: u32) -> Result<DxfDoc, JsError> {
        let (drawing, pages) = dwf::decode(data, page).map_err(|e| JsError::new(&e))?;
        Ok(DxfDoc { drawing, pages })
    }

    /// Opens picture `page` (0-based) of a binary or clear-text CGM file.
    #[wasm_bindgen(js_name = openCgm)]
    pub fn open_cgm(data: &[u8], page: u32) -> Result<DxfDoc, JsError> {
        let (drawing, pages) = cgm::decode(data, page).map_err(|e| JsError::new(&e))?;
        Ok(DxfDoc { drawing, pages })
    }

    #[wasm_bindgen(getter, js_name = pageCount)]
    pub fn page_count(&self) -> u32 {
        self.pages
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> f64 {
        self.drawing.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> f64 {
        self.drawing.height
    }

    /// World coordinates of drawing (0, 0), the top-left of the extents: minimum X, maximum Y.
    pub fn origin(&self) -> Vec<f64> {
        self.drawing.origin.to_vec()
    }

    #[wasm_bindgen(getter)]
    pub fn units(&self) -> String {
        self.drawing.units.into()
    }

    /// Format-specific properties as label, value, label, value, …
    pub fn info(&self) -> Vec<String> {
        flatten(&self.drawing.info)
    }

    /// Path vertices as x, y pairs.
    #[wasm_bindgen(js_name = pathPoints)]
    pub fn path_points(&self) -> Vec<f64> {
        self.drawing.paths.iter().flat_map(|p| p.points.iter().flatten().copied()).collect()
    }

    /// Vertex count of each path.
    #[wasm_bindgen(js_name = pathLengths)]
    pub fn path_lengths(&self) -> Vec<u32> {
        self.drawing.paths.iter().map(|p| p.points.len() as u32).collect()
    }

    #[wasm_bindgen(js_name = pathColors)]
    pub fn path_colors(&self) -> Vec<u32> {
        self.drawing.paths.iter().map(|p| p.color).collect()
    }

    /// Line width of each path in drawing units; 0 = hairline.
    #[wasm_bindgen(js_name = pathWidths)]
    pub fn path_widths(&self) -> Vec<f64> {
        self.drawing.paths.iter().map(|p| p.width).collect()
    }

    /// Eight numbers per arc: centre x, y, u x, y, v x, y, t0, t1 (see `dxf::Arc`).
    pub fn arcs(&self) -> Vec<f64> {
        self.drawing.arcs.iter().flat_map(|a| [a.centre[0], a.centre[1], a.u[0], a.u[1], a.v[0], a.v[1], a.t0, a.t1]).collect()
    }

    #[wasm_bindgen(js_name = arcColors)]
    pub fn arc_colors(&self) -> Vec<u32> {
        self.drawing.arcs.iter().map(|a| a.color).collect()
    }

    #[wasm_bindgen(js_name = arcWidths)]
    pub fn arc_widths(&self) -> Vec<f64> {
        self.drawing.arcs.iter().map(|a| a.width).collect()
    }

    /// Index into the dash patterns of each path; 0 = solid.
    #[wasm_bindgen(js_name = pathDashes)]
    pub fn path_dashes(&self) -> Vec<u32> {
        self.drawing.paths.iter().map(|p| p.dash).collect()
    }

    #[wasm_bindgen(js_name = arcDashes)]
    pub fn arc_dashes(&self) -> Vec<u32> {
        self.drawing.arcs.iter().map(|a| a.dash).collect()
    }

    /// All dash patterns' on and off lengths, in drawing units, one pattern after another.
    #[wasm_bindgen(js_name = dashLengths)]
    pub fn dash_lengths(&self) -> Vec<f64> {
        self.drawing.dashes.iter().flatten().copied().collect()
    }

    /// Number of lengths in each dash pattern; the first (solid) has none.
    #[wasm_bindgen(js_name = dashCounts)]
    pub fn dash_counts(&self) -> Vec<u32> {
        self.drawing.dashes.iter().map(|d| d.len() as u32).collect()
    }

    /// Ring vertices of all fills as x, y pairs.
    #[wasm_bindgen(js_name = fillPoints)]
    pub fn fill_points(&self) -> Vec<f64> {
        self.drawing.fills.iter().flat_map(|f| f.rings.iter().flatten().flatten().copied()).collect()
    }

    /// Vertex count of each ring, fill after fill.
    #[wasm_bindgen(js_name = fillRingLengths)]
    pub fn fill_ring_lengths(&self) -> Vec<u32> {
        self.drawing.fills.iter().flat_map(|f| f.rings.iter().map(|r| r.len() as u32)).collect()
    }

    /// Ring count of each fill.
    #[wasm_bindgen(js_name = fillRingCounts)]
    pub fn fill_ring_counts(&self) -> Vec<u32> {
        self.drawing.fills.iter().map(|f| f.rings.len() as u32).collect()
    }

    #[wasm_bindgen(js_name = fillColors)]
    pub fn fill_colors(&self) -> Vec<u32> {
        self.drawing.fills.iter().map(|f| f.color).collect()
    }

    #[wasm_bindgen(js_name = fillLayers)]
    pub fn fill_layers(&self) -> Vec<u32> {
        self.drawing.fills.iter().map(|f| f.layer).collect()
    }

    /// 1 for the even-odd fill rule, 0 for non-zero.
    #[wasm_bindgen(js_name = fillEvenOdd)]
    pub fn fill_even_odd(&self) -> Vec<u8> {
        self.drawing.fills.iter().map(|f| f.even_odd as u8).collect()
    }

    /// 1 for fills drawn over the lines (wipeouts), 0 for the rest.
    #[wasm_bindgen(js_name = fillMasks)]
    pub fn fill_masks(&self) -> Vec<u8> {
        self.drawing.fills.iter().map(|f| f.mask as u8).collect()
    }

    /// Six numbers per fill: gradient kind (0 none, 1 linear, 2 radial), points a and b (see
    /// `dxf::Gradient`), and its number of stops in `fillGradientStops()`.
    #[wasm_bindgen(js_name = fillGradients)]
    pub fn fill_gradients(&self) -> Vec<f64> {
        let geometry = |g: &dxf::Gradient| [if g.radial { 2.0 } else { 1.0 }, g.a[0], g.a[1], g.b[0], g.b[1], g.stops.len() as f64];
        self.drawing.fills.iter().flat_map(|f| f.gradient.as_ref().map_or([0.0; 6], geometry)).collect()
    }

    /// Offset and colour of each gradient stop, fill after fill.
    #[wasm_bindgen(js_name = fillGradientStops)]
    pub fn fill_gradient_stops(&self) -> Vec<f64> {
        self.drawing.fills.iter().filter_map(|f| f.gradient.as_ref()).flat_map(|g| g.stops.iter().flat_map(|&(t, c)| [t, c as f64])).collect()
    }

    /// The page as top-left x, y, width and height, or empty if there is none.
    pub fn background(&self) -> Vec<f64> {
        self.drawing.background.iter().flat_map(|b| [b.pos[0], b.pos[1], b.size[0], b.size[1]]).collect()
    }

    #[wasm_bindgen(getter, js_name = backgroundColor)]
    pub fn background_color(&self) -> u32 {
        self.drawing.background.as_ref().map_or(dxf::BACKGROUND, |b| b.color)
    }

    /// Four numbers per image: top-left x, y, pixel size in drawing units, layer.
    #[wasm_bindgen(js_name = imagePlacements)]
    pub fn image_placements(&self) -> Vec<f64> {
        self.drawing.images.iter().flat_map(|i| [i.pos[0], i.pos[1], i.px, i.layer as f64]).collect()
    }

    /// Moves the images out as `RasterDoc`s (in `imagePlacements()` order), which the caller frees.
    #[wasm_bindgen(js_name = takeImages)]
    pub fn take_images(&mut self) -> Vec<RasterDoc> {
        std::mem::take(&mut self.drawing.images)
            .into_iter()
            .map(|i| {
                let image = match i.image {
                    tiff::Image::Bilevel(bitmap) => Image::Bilevel(Raster::new(bitmap)),
                    tiff::Image::Color(rgba) => Image::Color(ColorRaster::new(rgba)),
                };
                RasterDoc { image, dpi: 0, info: Vec::new(), out: Vec::new() }
            })
            .collect()
    }

    /// Eight numbers per text: anchor x, y, baseline vector x, y, up vector x, y, halign, valign (see `dxf::Text`).
    pub fn texts(&self) -> Vec<f64> {
        self.drawing
            .texts
            .iter()
            .flat_map(|t| [t.pos[0], t.pos[1], t.x[0], t.x[1], t.up[0], t.up[1], t.halign as f64, t.valign as f64])
            .collect()
    }

    #[wasm_bindgen(js_name = textStrings)]
    pub fn text_strings(&self) -> Vec<String> {
        self.drawing.texts.iter().map(|t| t.text.clone()).collect()
    }

    /// Width each text is shrunk to fit if wider, in drawing units; 0 for none.
    #[wasm_bindgen(js_name = textFits)]
    pub fn text_fits(&self) -> Vec<f64> {
        self.drawing.texts.iter().map(|t| t.fit).collect()
    }

    /// Width each text's lines are word-wrapped at, in drawing units; 0 for none.
    #[wasm_bindgen(js_name = textWraps)]
    pub fn text_wraps(&self) -> Vec<f64> {
        self.drawing.texts.iter().map(|t| t.wrap).collect()
    }

    /// Font of each text: family (0 sans-serif, 1 serif, 2 monospace) plus 4 for italic and 8 for bold.
    #[wasm_bindgen(js_name = textFonts)]
    pub fn text_fonts(&self) -> Vec<u8> {
        self.drawing.texts.iter().map(|t| t.font).collect()
    }

    /// Two numbers per text: background mask colour (-1 for none) and how far it reaches beyond the text, in drawing units.
    #[wasm_bindgen(js_name = textMasks)]
    pub fn text_masks(&self) -> Vec<f64> {
        self.drawing.texts.iter().flat_map(|t| t.mask.map_or([-1.0, 0.0], |(c, margin)| [c as f64, margin])).collect()
    }

    #[wasm_bindgen(js_name = textColors)]
    pub fn text_colors(&self) -> Vec<u32> {
        self.drawing.texts.iter().map(|t| t.color).collect()
    }

    /// Index into `layerNames()` of each path, arc and text.
    #[wasm_bindgen(js_name = pathLayers)]
    pub fn path_layers(&self) -> Vec<u32> {
        self.drawing.paths.iter().map(|p| p.layer).collect()
    }

    #[wasm_bindgen(js_name = arcLayers)]
    pub fn arc_layers(&self) -> Vec<u32> {
        self.drawing.arcs.iter().map(|a| a.layer).collect()
    }

    #[wasm_bindgen(js_name = textLayers)]
    pub fn text_layers(&self) -> Vec<u32> {
        self.drawing.texts.iter().map(|t| t.layer).collect()
    }

    #[wasm_bindgen(js_name = layerNames)]
    pub fn layer_names(&self) -> Vec<String> {
        self.drawing.layers.iter().map(|l| l.name.clone()).collect()
    }

    #[wasm_bindgen(js_name = layerColors)]
    pub fn layer_colors(&self) -> Vec<u32> {
        self.drawing.layers.iter().map(|l| l.color).collect()
    }

    /// 1 if the layer is on in the file, 0 if it is off or frozen.
    #[wasm_bindgen(js_name = layerVisible)]
    pub fn layer_visible(&self) -> Vec<u8> {
        self.drawing.layers.iter().map(|l| l.visible as u8).collect()
    }
}

/// Colour value meaning "the drawing's foreground" (ACI 7) in `DxfDoc` colour arrays.
#[wasm_bindgen(js_name = dxfForeground)]
pub fn dxf_foreground() -> u32 {
    dxf::FOREGROUND
}

/// Colour value meaning "the paper colour" in `DxfDoc` colour arrays.
#[wasm_bindgen(js_name = dxfBackground)]
pub fn dxf_background() -> u32 {
    dxf::BACKGROUND
}
