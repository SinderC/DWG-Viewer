//! DXF (ASCII) model-space geometry for vector display.
//!
//! Blocks are expanded into world coordinates. Lines and polylines become paths; circles, arcs,
//! ellipses and polyline bulges stay exact as affine images of the unit circle, so they are sharp
//! at any zoom and keep their centres and radii (for later measuring and snapping). Splines are
//! flattened. Output coordinates are relative to the top-left of the drawing extents with Y down,
//! like image pixels, which keeps them small enough for the canvas' single-precision paths.
//!
//! The output side (`World`, `Affine`, `style`, text and spline helpers) is shared with `dwg` and `hpgl`.

pub(crate) mod acis;
mod color;
pub(crate) mod dim;
pub(crate) mod parse;
pub(crate) mod shapes;

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;

use acadrust::entities::solid3d::AcisData;
pub(crate) use color::aci;
pub use color::{BACKGROUND, FOREGROUND};
use dim::{Dim, DimStyle, DimText};
use parse::{Pair, Record};

use crate::tiff::Image;

pub(crate) type P = [f64; 2];

/// Maximum INSERT nesting; deeper (or self-referencing) blocks are skipped.
pub(crate) const MAX_DEPTH: u32 = 16;
/// Segments per knot span when flattening splines.
const SPLINE_STEPS: usize = 16;

pub struct Path {
    pub points: Vec<P>,
    pub color: u32,
    /// Line width in drawing units; 0 is a hairline.
    pub width: f64,
    /// Index into `Drawing::dashes`.
    pub dash: u32,
    /// Index into `Drawing::layers`.
    pub layer: u32,
    /// Line weight in 1/100 mm, drawn in screen pixels when line weights are shown; 0 for none.
    pub weight: u8,
}

/// The points `centre + u·cos t + v·sin t` for `t` from `t0` to `t1` (which may be less than `t0`).
pub struct Arc {
    pub centre: P,
    pub u: P,
    pub v: P,
    pub t0: f64,
    pub t1: f64,
    pub color: u32,
    /// Line width in drawing units; 0 is a hairline.
    pub width: f64,
    /// Index into `Drawing::dashes`.
    pub dash: u32,
    /// Index into `Drawing::layers`.
    pub layer: u32,
    /// As for [`Path::weight`].
    pub weight: u8,
}

/// A line of text (lines separated by `\n`). Glyph space is spanned by `x` (the baseline direction,
/// length = height × width factor) and `up` (length = cap height) from the anchor `pos`.
pub struct Text {
    pub pos: P,
    pub x: P,
    pub up: P,
    /// 0 left, 1 centre, 2 right.
    pub halign: u8,
    /// 0 baseline, 1 bottom, 2 middle, 3 top.
    pub valign: u8,
    pub text: String,
    pub color: u32,
    /// Index into `Drawing::layers`.
    pub layer: u32,
    /// Width the text is shrunk to fit if wider, in drawing units; 0 for none.
    pub fit: f64,
    /// Width the text's lines are word-wrapped at, in drawing units; 0 for none.
    pub wrap: f64,
    /// Font family and style: `FONT_SANS`, `FONT_SERIF` or `FONT_MONO`, plus `FONT_ITALIC` and `FONT_BOLD`.
    pub font: u8,
    /// Background mask: its colour and how far it reaches beyond the text, in drawing units.
    pub mask: Option<(u32, f64)>,
}

pub const FONT_SANS: u8 = 0;
pub const FONT_SERIF: u8 = 1;
pub const FONT_MONO: u8 = 2;
pub const FONT_ITALIC: u8 = 4;
pub const FONT_BOLD: u8 = 8;

/// How a text is set beyond its frame: the widths it must fit in and wrap at (0 for none, in
/// the frame's units) and its font (see `Text::font`).
#[derive(Clone, Copy, Default)]
pub(crate) struct TextStyle {
    pub fit: f64,
    pub wrap: f64,
    pub font: u8,
}

/// A filled area: the rings are subpaths of one shape, filled with the even-odd or non-zero rule.
pub struct Fill {
    pub rings: Vec<Vec<P>>,
    pub even_odd: bool,
    pub color: u32,
    /// Index into `Drawing::layers`.
    pub layer: u32,
    /// Drawn over the lines (a WIPEOUT) instead of under them.
    pub mask: bool,
    /// Filled with this instead of `color`.
    pub gradient: Option<Gradient>,
}

/// A gradient fill: linear from `a` to `b`, or radial about `a` out to `b`, with colour stops (offset 0–1, colour).
pub struct Gradient {
    pub radial: bool,
    pub a: P,
    pub b: P,
    pub stops: Vec<(f64, u32)>,
}

/// A raster image placed in the drawing: `pos` is its top-left corner, `px` the size of a pixel in drawing units.
pub struct PlacedImage {
    pub image: Image,
    pub pos: P,
    pub px: f64,
    /// Index into `Drawing::layers`.
    pub layer: u32,
}

pub struct Drawing {
    pub width: f64,
    pub height: f64,
    /// World coordinates of output (0, 0): the drawing's minimum X and maximum Y.
    pub origin: P,
    /// Length unit from `$INSUNITS`, or "" if unitless.
    pub units: &'static str,
    pub paths: Vec<Path>,
    pub arcs: Vec<Arc>,
    pub texts: Vec<Text>,
    pub fills: Vec<Fill>,
    pub images: Vec<PlacedImage>,
    /// Dash patterns of the paths and arcs, as alternating on and off lengths in drawing units;
    /// the first, for solid lines, is empty.
    pub dashes: Vec<Vec<f64>>,
    /// The page, if the format has one, painted before everything else.
    pub background: Option<Background>,
    /// Layers in name order, with their visibility as saved in the file.
    pub layers: Vec<Layer>,
    /// File properties as (label, value) rows for display.
    pub info: Vec<(String, String)>,
}

/// A page: top-left corner, width and height, and colour.
pub struct Background {
    pub pos: P,
    pub size: P,
    pub color: u32,
}

/// Colour, layer, line width (in drawing units, 0 = hairline), dash pattern (index into
/// `Drawing::dashes`, 0 = solid) and line weight (see [`Path::weight`]) of a shape.
#[derive(Clone, Copy)]
pub(crate) struct Pen {
    pub color: u32,
    pub layer: u32,
    pub width: f64,
    pub dash: u32,
    pub weight: u8,
}

/// Parses an ASCII DXF file and expands its model space.
pub fn decode(data: &[u8]) -> Result<Drawing, String> {
    let text = parse::decode_text(data)?;
    let pairs = parse::pairs(&text)?;
    let records = parse::records(&pairs);

    let mut header: &[Pair] = &[];
    let mut layers = Vec::new();
    let mut linetypes = Vec::new();
    let mut styles = Vec::new();
    let mut dimstyles = Vec::new();
    let mut blocks = HashMap::new();
    let mut entities = Vec::new();
    let mut acis = HashMap::new();
    let mut found_section = false;
    let mut i = 0;
    while i < records.len() {
        let rec = records[i];
        i += 1;
        if rec.kind != "SECTION" {
            continue;
        }
        found_section = true;
        let end = records[i..].iter().position(|r| r.kind == "ENDSEC").map_or(records.len(), |n| i + n);
        let body = &records[i..end];
        match rec.get(2).map(str::trim) {
            // Header variables have no `0` groups, so they all sit in the SECTION record.
            Some("HEADER") => header = rec.pairs,
            Some("TABLES") => {
                layers.extend(body.iter().filter(|r| r.kind == "LAYER").map(layer));
                linetypes.extend(body.iter().filter(|r| r.kind == "LTYPE").copied());
                styles.extend(body.iter().filter(|r| r.kind == "STYLE").copied());
                dimstyles.extend(body.iter().filter(|r| r.kind == "DIMSTYLE").copied());
            }
            Some("BLOCKS") => blocks = parse_blocks(body),
            Some("ENTITIES") => entities = group(body),
            Some("ACDSDATA") => acis = acds_data(body),
            _ => {}
        }
        i = end + 1;
    }
    if !found_section {
        return Err("Not a DXF file".into());
    }

    let units = units_name(header_var(header, "$INSUNITS").and_then(|v| v.parse().ok()).unwrap_or(0));
    let model_space = entities.iter().filter(|e| e.rec.i(67, 0) != 1).count();
    let mut info = vec![("DXF version".to_string(), version(header_var(header, "$ACADVER")))];
    info.extend(header_var(header, "$DWGCODEPAGE").map(|v| ("Code page".to_string(), v.to_string())));
    info.extend(summary(units, &layers, blocks.keys().map(String::as_str), model_space));

    // Dimension styles over the header's dimension variables, which also cover styles not listed.
    let header_dim = |code: i32| DIM_VARS.iter().find(|v| v.0 == code).and_then(|v| header_var(header, v.1)).and_then(|v| v.parse().ok());
    let dim_style = DimStyle::from_codes(header_dim, header_var(header, "$DIMPOST"), &DimStyle::default());
    let dim_styles = dimstyles.iter().map(|r| (r.name(2), DimStyle::from_codes(|code| r.get(code).and_then(|v| v.trim().parse().ok()), r.get(3), &dim_style))).collect();
    let mut b = Builder { blocks: &blocks, world: World::new(layers), dim_styles, dim_style, acis };
    b.world.default_weight = header_var(header, "$LWDEFAULT").and_then(|v| v.trim().parse().ok()).filter(|w: &u8| *w <= 211).unwrap_or(25);
    let ltscale = header_var(header, "$LTSCALE").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    // Shape files of the text styles, by upper-case handle.
    let shape_files: HashMap<String, &str> = styles.iter().map(|r| (r.name(5), r.get(3).unwrap_or(""))).collect();
    for lt in &linetypes {
        if let Some(elements) = linetype_elements(lt, &shape_files) {
            b.world.add_linetype(&lt.name(2), elements.iter().map(|e| e.scaled(ltscale)).collect());
        }
    }
    for e in &entities {
        b.entity(e, &Ctx::ROOT);
    }
    b.world.finish(units, info)
}

/// Info rows common to DXF and DWG.
pub(crate) fn summary<'a>(units: &str, layers: &[Layer], block_names: impl Iterator<Item = &'a str>, model_space: usize) -> [(String, String); 4] {
    let hidden_layers = layers.iter().filter(|l| !l.visible).count();
    [
        ("Units".to_string(), if units.is_empty() { "Unitless".to_string() } else { units.to_string() }),
        ("Layers".to_string(), format!("{} ({hidden_layers} off or frozen)", layers.len())),
        ("Blocks".to_string(), block_names.filter(|k| !k.starts_with('*')).count().to_string()),
        ("Model-space entities".to_string(), model_space.to_string()),
    ]
}

/// Value of header variable `name` (the group after its `9` group).
fn header_var<'a>(pairs: &[Pair<'a>], name: &str) -> Option<&'a str> {
    pairs.windows(2).find(|w| w[0].code == 9 && w[0].value.trim() == name).map(|w| w[1].value.trim())
}

/// `$ACADVER` with the AutoCAD release it belongs to.
pub(crate) fn version(acadver: Option<&str>) -> String {
    let Some(v) = acadver else { return "Unknown (no $ACADVER)".into() };
    let release = match v {
        "AC1006" => "R10",
        "AC1009" => "R11/R12",
        "AC1012" => "R13",
        "AC1014" => "R14",
        "AC1015" => "AutoCAD 2000",
        "AC1018" => "AutoCAD 2004",
        "AC1021" => "AutoCAD 2007",
        "AC1024" => "AutoCAD 2010",
        "AC1027" => "AutoCAD 2013",
        "AC1032" => "AutoCAD 2018",
        _ => return v.to_string(),
    };
    format!("{v} ({release})")
}

/// Model-space shapes in world coordinates (Y up), before `finish` moves them to output coordinates.
pub(crate) struct World<'a> {
    paths: Vec<Path>,
    arcs: Vec<Arc>,
    texts: Vec<Text>,
    fills: Vec<Fill>,
    images: Vec<PlacedImage>,
    /// RAYs and XLINEs (true: unbounded both ways) by base point and direction, in world coordinates.
    rays: Vec<(P, P, bool, Pen)>,
    /// Page corners (world coordinates) and colour.
    page: Option<(P, P, u32)>,
    /// Entity types that are not drawn, with how often they occur (block contents once per insert).
    skipped: BTreeMap<&'a str, usize>,
    layers: Vec<Layer>,
    /// Upper-case layer name → index in `layers`.
    layer_ids: HashMap<String, u32>,
    /// Canvas dash patterns of the linetypes, at linetype scale 1; the first is solid. Complex
    /// linetypes have an empty (solid) pattern and their elements in `complex`.
    linetypes: Vec<Vec<f64>>,
    complex: HashMap<u32, Vec<LinetypeElement>>,
    /// Upper-case linetype name → index in `linetypes`.
    linetype_ids: HashMap<String, u32>,
    /// Scaled patterns handed out as `Pen::dash`, and their (linetype, scale bits) → index.
    dashes: Vec<Vec<f64>>,
    dash_ids: HashMap<(u32, u64), u32>,
    /// Scaled elements of the complex linetypes, by `Pen::dash`.
    complex_dashes: HashMap<u32, Vec<LinetypeElement>>,
    /// Line weight for DEFAULT ($LWDEFAULT), in 1/100 mm.
    pub(crate) default_weight: u8,
}

impl<'a> World<'a> {
    /// `layers` is the file's layer table; layers used but not listed are added as they are met.
    pub(crate) fn new(mut layers: Vec<Layer>) -> Self {
        layers.sort_by_cached_key(|l| l.name.to_uppercase());
        layers.dedup_by(|a, b| a.name.to_uppercase() == b.name.to_uppercase());
        let layer_ids = layers.iter().enumerate().map(|(i, l)| (l.name.to_uppercase(), i as u32)).collect();
        let (paths, arcs, texts, fills, images, rays, skipped, linetype_ids, dash_ids) = Default::default();
        let (linetypes, dashes, complex, complex_dashes) = (vec![Vec::new()], vec![Vec::new()], HashMap::new(), HashMap::new());
        World { paths, arcs, texts, fills, images, rays, page: None, skipped, layers, layer_ids, linetypes, complex, linetype_ids, dashes, dash_ids, complex_dashes, default_weight: 25 }
    }

    /// Adds linetype `name` with its AutoCAD pattern: dashes positive, gaps negative, dots 0,
    /// any of them followed by a shape.
    pub(crate) fn add_linetype(&mut self, name: &str, elements: Vec<LinetypeElement>) {
        let id = self.linetypes.len() as u32;
        if elements.iter().any(|e| e.shape.is_some()) {
            self.complex.insert(id, elements);
            self.linetypes.push(Vec::new());
        } else {
            let dash = canvas_dash(elements.iter().map(|e| e.length));
            if dash.is_empty() {
                return;
            }
            self.linetypes.push(dash);
        }
        self.linetype_ids.insert(name.trim().to_uppercase(), id);
    }

    /// Index of linetype `name`, or 0 (solid) if it is continuous or not known.
    fn linetype_id(&self, name: &str) -> u32 {
        self.linetype_ids.get(&name.trim().to_uppercase()).copied().unwrap_or(0)
    }

    /// `Pen::dash` for `linetype` scaled by `scale`.
    fn dash(&mut self, linetype: u32, scale: f64) -> u32 {
        if linetype == 0 || !(scale > 0.0) {
            return 0;
        }
        let next = self.dashes.len() as u32;
        let id = *self.dash_ids.entry((linetype, scale.to_bits())).or_insert(next);
        if id == next {
            self.dashes.push(self.linetypes[linetype as usize].iter().map(|v| v * scale).collect());
            if let Some(elements) = self.complex.get(&linetype) {
                self.complex_dashes.insert(id, elements.iter().map(|e| e.scaled(scale)).collect());
            }
        }
        id
    }

    fn layer_id(&mut self, name: &str) -> u32 {
        self.layer(name, FOREGROUND)
    }

    /// Index of layer `name`, added with colour `color` if it is not known yet.
    pub(crate) fn layer(&mut self, name: &str, color: u32) -> u32 {
        let key = name.to_uppercase();
        if let Some(&id) = self.layer_ids.get(&key) {
            return id;
        }
        let id = self.layers.len() as u32;
        self.layers.push(Layer { name: name.to_string(), color, visible: true, linetype: String::new(), weight: -3 });
        self.layer_ids.insert(key, id);
        id
    }

    /// Sets the page: opposite corners in world coordinates and its colour. It is always part of the extents.
    pub(crate) fn page(&mut self, a: P, b: P, color: u32) {
        self.page = Some((a, b, color));
    }

    pub(crate) fn skip(&mut self, kind: &'a str) {
        *self.skipped.entry(kind).or_default() += 1;
    }

    pub(crate) fn path(&mut self, points: Vec<P>, m: &Affine, pen: Pen) {
        if points.len() < 2 {
            return;
        }
        let points: Vec<P> = points.into_iter().map(|p| m.apply(p)).collect();
        if let Some(elements) = self.complex_dashes.get(&pen.dash) {
            for points in complex_strokes(&points, elements) {
                self.paths.push(Path { points, color: pen.color, width: pen.width, dash: 0, layer: pen.layer, weight: pen.weight });
            }
        } else {
            self.paths.push(Path { points, color: pen.color, width: pen.width, dash: pen.dash, layer: pen.layer, weight: pen.weight });
        }
    }

    /// Rings with fewer than three points are dropped.
    pub(crate) fn fill(&mut self, rings: Vec<Vec<P>>, even_odd: bool, m: &Affine, pen: Pen) {
        let rings: Vec<Vec<P>> = rings.into_iter().filter(|r| r.len() >= 3).map(|r| r.into_iter().map(|p| m.apply(p)).collect()).collect();
        if !rings.is_empty() {
            self.fills.push(Fill { rings, even_odd, color: pen.color, layer: pen.layer, mask: false, gradient: None });
        }
    }

    /// A SOLID or TRACE: corners in zig-zag order (the fourth equal to the third for a triangle), filled.
    pub(crate) fn solid(&mut self, [a, b, c, d]: [P; 4], m: &Affine, pen: Pen) {
        self.fill(vec![vec![a, b, d, c]], false, m, pen);
    }

    /// A WIPEOUT: `ring` filled with the paper colour over the lines drawn so far.
    pub(crate) fn wipeout(&mut self, ring: Vec<P>, m: &Affine, layer: u32) {
        let pen = Pen { color: BACKGROUND, layer, width: 0.0, dash: 0, weight: 0 };
        self.fill(vec![ring], false, m, pen);
        if let Some(f) = self.fills.last_mut() {
            f.mask = true;
        }
    }

    /// A gradient-filled hatch: AutoCAD gradient `name` at `angle` from `c1` to `c2`, over the bounding box of `rings`.
    pub(crate) fn gradient(&mut self, rings: Vec<Vec<P>>, name: &str, angle: f64, c1: u32, c2: u32, m: &Affine, pen: Pen) {
        self.fill(rings, true, m, pen);
        let Some(fill) = self.fills.last_mut() else { return };
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in fill.rings.iter().flatten() {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        let radius = (hi[0] - lo[0]).hypot(hi[1] - lo[1]) / 2.0;
        let d = m.linear([angle.cos(), angle.sin()]);
        let d = [d[0] / d[0].hypot(d[1]).max(f64::MIN_POSITIVE), d[1] / d[0].hypot(d[1]).max(f64::MIN_POSITIVE)];
        // The box's extent along the gradient direction, about its centre.
        let reach = ((hi[0] - lo[0]) * d[0].abs() + (hi[1] - lo[1]) * d[1].abs()) / 2.0;
        let along = |t: f64| [centre[0] + d[0] * t, centre[1] + d[1] * t];
        let name = name.trim().to_uppercase();
        let (inverse, base) = match name.strip_prefix("INV") {
            Some(base) => (true, base),
            None => (false, name.as_str()),
        };
        let (c1, c2) = if inverse { (c2, c1) } else { (c1, c2) };
        fill.gradient = Some(match base {
            "CYLINDER" => Gradient { radial: false, a: along(-reach), b: along(reach), stops: vec![(0.0, c1), (0.5, c2), (1.0, c1)] },
            "SPHERICAL" => Gradient { radial: true, a: centre, b: [centre[0] + radius, centre[1]], stops: vec![(0.0, c2), (1.0, c1)] },
            // Lit from below the middle of the side opposite the gradient direction.
            "HEMISPHERICAL" => Gradient { radial: true, a: along(-reach), b: along(reach), stops: vec![(0.0, c2), (1.0, c1)] },
            "CURVED" => Gradient { radial: false, a: along(-reach), b: along(reach), stops: vec![(0.0, c2), (0.3, c2), (1.0, c1)] },
            _ => Gradient { radial: false, a: along(-reach), b: along(reach), stops: vec![(0.0, c1), (1.0, c2)] },
        });
    }

    /// A hatch over the even-odd area of `rings`: filled if `pattern` is None, otherwise drawn as
    /// each pattern line family clipped to it. Patterns too dense to draw line by line are filled.
    pub(crate) fn hatch(&mut self, rings: Vec<Vec<P>>, pattern: Option<&[HatchLine]>, m: &Affine, pen: Pen) {
        let pen = Pen { dash: 0, ..pen };
        let edges: Vec<(P, P)> = rings.iter().flat_map(|r| r.iter().copied().zip(r.iter().copied().cycle().skip(1))).collect();
        // Each family's direction and normal, and the range of k whose line can cross the boundary.
        let families: Vec<_> = pattern
            .unwrap_or_default()
            .iter()
            .filter_map(|l| {
                let (s, c) = l.angle.sin_cos();
                let (d, n) = ([c, s], [-s, c]);
                let step = dot(l.offset, n);
                // Lines with no spacing between them do not hatch anything.
                if !(step.abs() > 1e-9 * dot(l.offset, l.offset).sqrt()) {
                    return None;
                }
                let k = |p: P| (dot(p, n) - dot(l.base, n)) / step;
                let (lo, hi) = edges.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(a, _)| (lo.min(k(a)), hi.max(k(a))));
                (lo <= hi).then(|| (l, d, n, lo.ceil() as i64, hi.floor() as i64))
            })
            .collect();
        let lines: i64 = families.iter().map(|f| (f.4 - f.3 + 1).max(0)).sum();
        if pattern.is_none() || lines > MAX_HATCH_LINES {
            return self.fill(rings, true, m, pen);
        }
        for (l, d, n, k0, k1) in families {
            for k in k0..=k1 {
                let o = [l.base[0] + l.offset[0] * k as f64, l.base[1] + l.offset[1] * k as f64];
                let rel = |p: P| [p[0] - o[0], p[1] - o[1]];
                // Where the line crosses the boundary, as distances along it from `o`.
                let mut ts: Vec<f64> = edges
                    .iter()
                    .filter_map(|&(a, b)| {
                        let (na, nb) = (dot(rel(a), n), dot(rel(b), n));
                        let (ta, tb) = (dot(rel(a), d), dot(rel(b), d));
                        ((na > 0.0) != (nb > 0.0)).then(|| ta + (tb - ta) * na / (na - nb))
                    })
                    .collect();
                ts.sort_by(f64::total_cmp);
                let at = |t: f64| [o[0] + d[0] * t, o[1] + d[1] * t];
                for inside in ts.chunks_exact(2) {
                    for (t0, t1) in dash_spans(inside[0], inside[1], &l.dashes) {
                        self.path(vec![at(t0), at(t1)], m, pen);
                    }
                }
            }
        }
    }

    /// Draws a multileader's lines and text. Returns each arrowhead's transform from arrow block
    /// units (tip at the origin, pointing along +X, size 1) and its block, for the caller to draw.
    pub(crate) fn multileader(&mut self, ml: &MultiLeader, m: &Affine, line: Pen, text: Pen) -> Vec<(Affine, Option<String>)> {
        if let Some(t) = &ml.text {
            self.push_text(t.at, t.angle, t.height, 1.0, mtext_align(1 + t.column as i64), parse::mtext_rich(&t.value, t.height), m, text);
        }
        let mut arrows = Vec::new();
        for root in &ml.roots {
            let [x, y] = root.connection;
            if ml.dogleg && root.landing > 0.0 {
                self.path(vec![root.connection, [x + root.direction[0] * root.landing, y + root.direction[1] * root.landing]], m, line);
            }
            for l in &root.lines {
                let points: Vec<P> = l.points.iter().copied().chain([root.connection]).collect();
                if let [tip, next, ..] = points[..] {
                    arrows.push((m.then(&arrow(tip, next, ml.arrow_size)), l.arrow.clone().or_else(|| ml.arrow.clone())));
                }
                self.path(points, m, line);
            }
        }
        arrows
    }

    /// The default arrowhead, closed and filled, in arrow block units.
    pub(crate) fn arrowhead(&mut self, m: &Affine, pen: Pen) {
        self.fill(vec![vec![[0.0, 0.0], [-1.0, 1.0 / 6.0], [-1.0, -1.0 / 6.0]]], false, m, pen);
    }

    /// A RAY from `base` along `dir`, or an XLINE through it if `both_ways`. It is cut off at
    /// `RAY_REACH` drawing sizes beyond the extents of everything else.
    pub(crate) fn ray(&mut self, base: P, dir: P, both_ways: bool, m: &Affine, pen: Pen) {
        let dir = m.linear(dir);
        if dir != [0.0; 2] {
            self.rays.push((m.apply(base), dir, both_ways, pen));
        }
    }

    /// A LEADER through `points` (a fitted curve if `spline`), with the default arrowhead of
    /// `arrow_size` at its first point unless that is 0.
    pub(crate) fn leader(&mut self, points: Vec<P>, spline: bool, arrow_size: f64, m: &Affine, pen: Pen) {
        if let [tip, next, ..] = points[..] {
            if arrow_size > 0.0 {
                self.arrowhead(&m.then(&arrow(tip, next, arrow_size)), pen);
            }
        }
        self.path(if spline { through(&points) } else { points }, m, pen);
    }

    /// `pos` is the top-left corner in world coordinates (Y up).
    pub(crate) fn image(&mut self, image: Image, pos: P, px: f64, layer: u32) {
        self.images.push(PlacedImage { image, pos, px, layer });
    }

    pub(crate) fn arc(&mut self, a: Arc, m: &Affine) {
        if self.complex_dashes.contains_key(&a.dash) {
            // Complex linetypes are laid out along the arc as a polyline.
            let steps = ((a.t1 - a.t0).abs() / (TAU / 64.0)).ceil().max(2.0) as usize;
            let points = (0..=steps).map(|i| a.at(a.t0 + (a.t1 - a.t0) * i as f64 / steps as f64)).collect();
            return self.path(points, m, Pen { color: a.color, width: a.width, dash: a.dash, layer: a.layer, weight: a.weight });
        }
        self.arcs.push(Arc { centre: m.apply(a.centre), u: m.linear(a.u), v: m.linear(a.v), ..a });
    }

    pub(crate) fn circle_arc(&mut self, centre: P, r: f64, start: f64, sweep: f64, m: &Affine, pen: Pen) {
        if r > 0.0 {
            self.arc(Arc { centre, u: [r, 0.0], v: [0.0, r], t0: start, t1: start + sweep, color: pen.color, width: pen.width, dash: pen.dash, layer: pen.layer, weight: pen.weight }, m);
        }
    }

    /// `ratio` is minor / major axis, negated for a flipped extrusion: the minor axis is the
    /// extrusion direction × the major axis.
    pub(crate) fn ellipse(&mut self, centre: P, u: P, ratio: f64, t0: f64, t1: f64, m: &Affine, pen: Pen) {
        self.arc(Arc { centre, u, v: [-u[1] * ratio, u[0] * ratio], t0, t1: t0 + sweep(t0, t1), color: pen.color, width: pen.width, dash: pen.dash, layer: pen.layer, weight: pen.weight }, m);
    }

    /// Straight runs become paths and bulged segments arcs; segments with a width become filled
    /// bands (see [`World::band`]).
    pub(crate) fn polyline(&mut self, verts: &[Vertex], closed: bool, m: &Affine, pen: Pen) {
        if verts.is_empty() {
            return;
        }
        let mut run = vec![verts[0].0];
        for i in 0..segment_count(verts, closed) {
            let (a, bulge, widths) = verts[i];
            let b = verts[(i + 1) % verts.len()].0;
            if bulge.abs() < 1e-9 && widths == [0.0; 2] {
                run.push(b);
                continue;
            }
            self.path(std::mem::replace(&mut run, vec![b]), m, pen);
            if widths != [0.0; 2] {
                self.band(verts, closed, i, m, pen);
            } else {
                let (centre, r, start, sweep) = bulge_arc(a, b, bulge);
                self.circle_arc(centre, r, start, sweep, m, pen);
            }
        }
        self.path(run, m, pen);
    }

    /// Segment `i` of a wide polyline as a filled outline. AutoCAD mitres a join, straight or
    /// curved, only where the width carries on unchanged into the neighbour; other ends are square.
    fn band(&mut self, verts: &[Vertex], closed: bool, i: usize, m: &Affine, pen: Pen) {
        let n = segment_count(verts, closed);
        let samples = segment_points(verts, i);
        let [w0, w1] = verts[i].2;
        let (before, after) = ((i + n - 1) % n, (i + 1) % n);
        let prev = ((i > 0 || closed) && w0 != 0.0 && verts[before].2[1] == w0).then(|| segment_points(verts, before).last().map(|s| s.1)).flatten();
        let next = ((i + 1 < n || closed) && w1 != 0.0 && verts[after].2[0] == w1).then(|| segment_points(verts, after).first().map(|s| s.1)).flatten();
        let last = samples.len().saturating_sub(1);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for (k, &(p, t)) in samples.iter().enumerate() {
            let half = (w0 + (w1 - w0) * k as f64 / last.max(1) as f64) / 2.0;
            let neighbour = if k == 0 { prev } else if k == last { next } else { None };
            let off = mitre(t, neighbour, half);
            left.push([p[0] + off[0], p[1] + off[1]]);
            right.push([p[0] - off[0], p[1] - off[1]]);
        }
        left.extend(right.into_iter().rev());
        self.fill(vec![left], false, m, pen);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_text(&mut self, pos: P, rotation: f64, height: f64, width: f64, align: (u8, u8), text: String, m: &Affine, pen: Pen) {
        self.styled_text(pos, rotation, height, width, align, text, TextStyle::default(), m, pen);
    }

    /// MTEXT, its lines word-wrapped at `wrap` (0 for none).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mtext(&mut self, pos: P, rotation: f64, height: f64, wrap: f64, align: (u8, u8), text: String, m: &Affine, pen: Pen) {
        self.styled_text(pos, rotation, height, 1.0, align, text, TextStyle { wrap: wrap.max(0.0), ..TextStyle::default() }, m, pen);
    }

    #[allow(clippy::too_many_arguments)]
    fn styled_text(&mut self, pos: P, rotation: f64, height: f64, width: f64, (halign, valign): (u8, u8), text: String, style: TextStyle, m: &Affine, pen: Pen) {
        if text.trim().is_empty() || height <= 0.0 {
            return;
        }
        let (s, c) = rotation.sin_cos();
        self.text_frame(pos, [c * height * width, s * height * width], [-s * height, c * height], (halign, valign), text, style, m, pen);
    }

    /// Text in glyph space spanned by `x` and `up` (see [`Text`]), which need not be perpendicular.
    #[allow(clippy::too_many_arguments)]
    /// `style.fit` is the width, along `x`, the text must fit in (0 for none).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_frame(&mut self, pos: P, x: P, up: P, (halign, valign): (u8, u8), text: String, style: TextStyle, m: &Affine, pen: Pen) {
        if !text.trim().is_empty() {
            let len = |v: P| v[0].hypot(v[1]);
            let scale = if len(x) > 0.0 { len(m.linear(x)) / len(x) } else { 0.0 };
            let (fit, wrap) = (style.fit * scale, style.wrap * scale);
            let text = Text { pos: m.apply(pos), x: m.linear(x), up: m.linear(up), halign, valign, text, color: pen.color, layer: pen.layer, fit, wrap, font: style.font, mask: None };
            self.texts.push(text);
        }
    }

    pub(crate) fn text_count(&self) -> usize {
        self.texts.len()
    }

    /// Gives the texts added since there were `start` a background mask of `color`, reaching
    /// `margin` (in the units before `m`) beyond them.
    pub(crate) fn mask_texts(&mut self, start: usize, color: u32, margin: f64, m: &Affine) {
        for t in self.texts.iter_mut().skip(start) {
            t.mask = Some((color, margin * m.length_scale()));
        }
    }

    pub(crate) fn finish(mut self, units: &'static str, mut info: Vec<(String, String)>) -> Result<Drawing, String> {
        if !self.skipped.is_empty() {
            let list: Vec<_> = self.skipped.iter().map(|(kind, n)| format!("{kind} × {n}")).collect();
            info.push(("Not drawn".to_string(), list.join(", ")));
        }
        // Fit to what is shown initially, unless the file hides everything.
        let Some((min, max)) = self.extents(|l| self.layers[l as usize].visible).or_else(|| self.extents(|_| true)) else {
            return Err("The drawing has no visible model-space geometry".into());
        };
        // A single point or a straight horizontal/vertical line still needs a non-zero size to fit.
        let size = (max[0] - min[0]).max(max[1] - min[1]).max(1e-9);
        let (width, height) = ((max[0] - min[0]).max(size * 1e-3), (max[1] - min[1]).max(size * 1e-3));
        let reach = RAY_REACH * size;
        for (base, dir, both_ways, pen) in std::mem::take(&mut self.rays) {
            let ends = clip_line(base, dir, if both_ways { f64::NEG_INFINITY } else { 0.0 }, [min[0] - reach, min[1] - reach], [max[0] + reach, max[1] + reach]);
            if let Some([a, b]) = ends {
                self.path(vec![a, b], &Affine::IDENTITY, pen);
            }
        }

        let origin = [min[0], max[1]];
        let pt = |p: P| [p[0] - origin[0], origin[1] - p[1]];
        let vec = |v: P| [v[0], -v[1]];
        Ok(Drawing {
            width,
            height,
            origin,
            units,
            paths: self.paths.into_iter().map(|p| Path { points: p.points.into_iter().map(pt).collect(), ..p }).collect(),
            arcs: self.arcs.into_iter().map(|a| Arc { centre: pt(a.centre), u: vec(a.u), v: vec(a.v), ..a }).collect(),
            texts: self.texts.into_iter().map(|t| Text { pos: pt(t.pos), x: vec(t.x), up: vec(t.up), ..t }).collect(),
            fills: self
                .fills
                .into_iter()
                .map(|f| Fill {
                    rings: f.rings.into_iter().map(|r| r.into_iter().map(pt).collect()).collect(),
                    gradient: f.gradient.map(|g| Gradient { a: pt(g.a), b: pt(g.b), ..g }),
                    ..f
                })
                .collect(),
            images: self.images.into_iter().map(|i| PlacedImage { pos: pt(i.pos), ..i }).collect(),
            dashes: self.dashes,
            background: self.page.map(|(a, b, color)| Background {
                pos: pt([a[0].min(b[0]), a[1].max(b[1])]),
                size: [(a[0] - b[0]).abs(), (a[1] - b[1]).abs()],
                color,
            }),
            layers: self.layers,
            info,
        })
    }

    /// Bounding box (min, max) of the shapes whose layer passes `include`, if there are any.
    fn extents(&self, include: impl Fn(u32) -> bool) -> Option<(P, P)> {
        let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        let mut grow = |p: P| {
            for k in 0..2 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        };
        self.paths.iter().filter(|p| include(p.layer)).flat_map(|p| &p.points).for_each(|&p| grow(p));
        for a in self.arcs.iter().filter(|a| include(a.layer)) {
            (0..=32).for_each(|k| grow(a.at(a.t0 + (a.t1 - a.t0) * k as f64 / 32.0)));
        }
        self.texts.iter().filter(|t| include(t.layer)).for_each(|t| grow(t.pos));
        self.fills.iter().filter(|f| include(f.layer)).flat_map(|f| f.rings.iter().flatten()).for_each(|&p| grow(p));
        for i in self.images.iter().filter(|i| include(i.layer)) {
            let (w, h) = i.image.size();
            grow(i.pos);
            grow([i.pos[0] + w as f64 * i.px, i.pos[1] - h as f64 * i.px]);
        }
        if let Some((a, b, _)) = self.page {
            grow(a);
            grow(b);
        }
        self.rays.iter().filter(|r| include(r.3.layer)).for_each(|r| grow(r.0));
        (min[0] <= max[0] && min[1] <= max[1]).then_some((min, max))
    }
}

impl Arc {
    fn at(&self, t: f64) -> P {
        let (s, c) = t.sin_cos();
        [self.centre[0] + self.u[0] * c + self.v[0] * s, self.centre[1] + self.u[1] * c + self.v[1] * s]
    }
}

/// Leader arrow size from DIMASZ and DIMSCALE (0 meaning 1, as in model space).
pub(crate) fn arrow_size(dimasz: f64, dimscale: f64) -> f64 {
    dimasz * if dimscale > 0.0 { dimscale } else { 1.0 }
}

/// How far, in drawing sizes, RAYs and XLINEs reach beyond the drawing's extents.
const RAY_REACH: f64 = 10.0;

/// The part of the line `base + t·dir`, `t` ≥ `t_min`, inside the box from `lo` to `hi`, as its two ends.
fn clip_line(base: P, dir: P, t_min: f64, lo: P, hi: P) -> Option<[P; 2]> {
    let (mut t0, mut t1) = (t_min, f64::INFINITY);
    for k in 0..2 {
        if dir[k] == 0.0 {
            if base[k] < lo[k] || base[k] > hi[k] {
                return None;
            }
        } else {
            let (a, b) = ((lo[k] - base[k]) / dir[k], (hi[k] - base[k]) / dir[k]);
            (t0, t1) = (t0.max(a.min(b)), t1.min(a.max(b)));
        }
    }
    let at = |t: f64| [base[0] + dir[0] * t, base[1] + dir[1] * t];
    (t0 < t1).then(|| [at(t0), at(t1)])
}

/// Transform from arrow block units (tip at the origin, pointing along +X, size 1) to an arrow
/// of `size` at `tip`, pointing away from `from`.
fn arrow(tip: P, from: P, size: f64) -> Affine {
    let angle = (tip[1] - from[1]).atan2(tip[0] - from[0]);
    Affine::translate(tip).then(&Affine::rotate(angle)).then(&Affine::scale(size, size))
}

/// Sweep from `t0` to `t1` counter-clockwise; equal angles mean a full turn.
pub(crate) fn sweep(t0: f64, t1: f64) -> f64 {
    let sweep = (t1 - t0).rem_euclid(TAU);
    if sweep == 0.0 { TAU } else { sweep }
}

/// A polyline vertex: its position, the bulge (tan(sweep / 4)) of the segment that starts there, and
/// that segment's start and end widths.
pub(crate) type Vertex = (P, f64, [f64; 2]);

fn segment_count(verts: &[Vertex], closed: bool) -> usize {
    if closed { verts.len() } else { verts.len().saturating_sub(1) }
}

/// Centre, radius, start angle and sweep of the arc from `a` to `b` with `bulge`.
fn bulge_arc(a: P, b: P, bulge: f64) -> (P, f64, f64, f64) {
    let ([x1, y1], [x2, y2]) = (a, b);
    let k = (1.0 - bulge * bulge) / (4.0 * bulge);
    let centre = [(x1 + x2) / 2.0 - (y2 - y1) * k, (y1 + y2) / 2.0 + (x2 - x1) * k];
    let r = (x2 - x1).hypot(y2 - y1) * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
    (centre, r, (y1 - centre[1]).atan2(x1 - centre[0]), 4.0 * bulge.atan())
}

/// Points along polyline segment `i`, each with the unit direction of travel there: the two ends
/// of a straight segment, or an arc flattened to steps of at most 1/64 turn. Empty if it has no length.
fn segment_points(verts: &[Vertex], i: usize) -> Vec<(P, P)> {
    let (a, bulge, _) = verts[i];
    let b = verts[(i + 1) % verts.len()].0;
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy);
    if len < 1e-12 {
        return Vec::new();
    }
    if bulge.abs() < 1e-9 {
        let t = [dx / len, dy / len];
        return vec![(a, t), (b, t)];
    }
    let (centre, r, start, sweep) = bulge_arc(a, b, bulge);
    let steps = ((sweep.abs() / (TAU / 64.0)).ceil() as usize).max(2);
    (0..=steps)
        .map(|k| {
            let (s, c) = (start + sweep * k as f64 / steps as f64).sin_cos();
            ([centre[0] + r * c, centre[1] + r * s], [-s * sweep.signum(), c * sweep.signum()])
        })
        .collect()
}

/// Offset to the left of direction `t` at distance `half`, along the bisector with the
/// neighbouring segment's direction there if it has one, so two bands meet without a notch.
fn mitre(t: P, neighbour: Option<P>, half: f64) -> P {
    let normal = [-t[1], t[0]];
    if let Some(u) = neighbour {
        let bisector = [normal[0] - u[1], normal[1] + u[0]];
        let len = bisector[0].hypot(bisector[1]);
        // Turns sharper than about 150° are not mitred, so the spike stays short.
        let cos = (bisector[0] * normal[0] + bisector[1] * normal[1]) / len;
        if len > 1e-9 && cos > 0.25 {
            return [bisector[0] / len * half / cos, bisector[1] / len * half / cos];
        }
    }
    [normal[0] * half, normal[1] * half]
}

/// One line of a hatch pattern, in the hatch's coordinates (already scaled and rotated, as files
/// store them): lines at `angle` through `base` + k·`offset` for every integer k, dashed by
/// `dashes` (an AutoCAD pattern that starts at each line's base point; empty for solid lines).
pub(crate) struct HatchLine {
    pub angle: f64,
    pub base: P,
    pub offset: P,
    pub dashes: Vec<f64>,
}

/// A MULTILEADER as DXF and DWG store it. Blocks are named by their block record handle (upper-case hex).
#[derive(Default)]
pub(crate) struct MultiLeader {
    pub roots: Vec<LeaderRoot>,
    /// Whether each root has a dogleg: a landing line from its connection point along its direction.
    pub dogleg: bool,
    pub arrow_size: f64,
    /// Arrowhead block, or None for the default closed filled arrow.
    pub arrow: Option<String>,
    pub text: Option<LeaderText>,
    /// Block content: block, insertion point, X and Y scale, rotation.
    pub block: Option<(String, P, P, f64)>,
}

/// Where a group of leader lines meets the content.
#[derive(Default)]
pub(crate) struct LeaderRoot {
    pub connection: P,
    pub direction: P,
    pub landing: f64,
    pub lines: Vec<LeaderLine>,
}

/// A leader line's points, from its arrow tip to before the connection point, and its own arrowhead block.
#[derive(Default)]
pub(crate) struct LeaderLine {
    pub points: Vec<P>,
    pub arrow: Option<String>,
}

/// MTEXT content of a multileader: the top of its first line is at `at`, on the side given by
/// `column` (0 left, 1 centre, 2 right).
pub(crate) struct LeaderText {
    pub value: String,
    pub at: P,
    pub angle: f64,
    pub height: f64,
    pub column: u8,
}

/// One element of a linetype: a dash (positive length), gap (negative) or dot (0), with the shape
/// drawn at its end, if any.
#[derive(Clone)]
pub(crate) struct LinetypeElement {
    pub length: f64,
    pub shape: Option<LinetypeShape>,
}

/// A shape of a complex linetype.
#[derive(Clone)]
pub(crate) struct LinetypeShape {
    /// Strokes in shape units (see `shapes::strokes`).
    pub strokes: Vec<Vec<P>>,
    pub scale: f64,
    /// Radians, from the line's direction, or from the X axis if `absolute`.
    pub rotation: f64,
    pub absolute: bool,
    /// Along the line and to its left, in drawing units.
    pub offset: P,
}

impl LinetypeElement {
    /// A plain dash, gap or dot.
    pub(crate) fn plain(length: f64) -> Self {
        LinetypeElement { length, shape: None }
    }

    /// The element with its lengths, offset and shape size multiplied by `k`.
    pub(crate) fn scaled(&self, k: f64) -> Self {
        let shape = self.shape.as_ref().map(|s| LinetypeShape { strokes: s.strokes.clone(), scale: s.scale * k, offset: [s.offset[0] * k, s.offset[1] * k], ..*s });
        LinetypeElement { length: self.length * k, shape }
    }
}

/// Most repeats of a complex linetype drawn along one path; finer ones become a solid line.
const MAX_LINETYPE_REPEATS: f64 = 5_000.0;

/// The strokes of complex linetype `elements` along `points`: its dashes and dots, and its shapes
/// placed at the ends of their elements. The pattern starts at the first point and repeats.
fn complex_strokes(points: &[P], elements: &[LinetypeElement]) -> Vec<Vec<P>> {
    let ends: Vec<f64> = points
        .windows(2)
        .scan(0.0, |len, w| {
            *len += (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
            Some(*len)
        })
        .collect();
    let total = ends.last().copied().unwrap_or(0.0);
    let period: f64 = elements.iter().map(|e| e.length.abs()).sum();
    if !(period > 0.0) || total / period > MAX_LINETYPE_REPEATS {
        return vec![points.to_vec()];
    }
    // Segment containing distance `t` along the path, and the point there.
    let segment = |t: f64| ends.partition_point(|&e| e < t).min(ends.len() - 1);
    let at = |t: f64| {
        let i = segment(t);
        let (a, b) = (points[i], points[i + 1]);
        let start = if i == 0 { 0.0 } else { ends[i - 1] };
        let f = if ends[i] > start { (t - start) / (ends[i] - start) } else { 0.0 };
        [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
    };
    let mut out = Vec::new();
    let mut pos = 0.0;
    while pos < total {
        for e in elements {
            let end = pos + e.length.abs();
            if e.length >= 0.0 && pos <= total {
                // The dash's points: its ends and the path's vertices between them.
                let (i, j) = (segment(pos), segment(end.min(total)));
                let mut dash = vec![at(pos)];
                dash.extend((i..j).map(|k| points[k + 1]));
                dash.push(at(end.min(total)));
                out.push(dash);
            }
            if let Some(shape) = e.shape.as_ref().filter(|_| end <= total) {
                let i = segment(end);
                let (a, b) = (points[i], points[i + 1]);
                let line = (b[1] - a[1]).atan2(b[0] - a[0]);
                let (ls, lc) = line.sin_cos();
                let o = at(end);
                let origin = [o[0] + shape.offset[0] * lc - shape.offset[1] * ls, o[1] + shape.offset[0] * ls + shape.offset[1] * lc];
                let (rs, rc) = (shape.rotation + if shape.absolute { 0.0 } else { line }).sin_cos();
                let place = |p: &P| [origin[0] + shape.scale * (p[0] * rc - p[1] * rs), origin[1] + shape.scale * (p[0] * rs + p[1] * rc)];
                out.extend(shape.strokes.iter().map(|s| s.iter().map(place).collect()));
            }
            pos = end;
        }
    }
    out
}

/// Most pattern lines drawn for one hatch; denser hatches are filled instead.
const MAX_HATCH_LINES: i64 = 20_000;
/// Most dashes drawn along one hatch line inside its boundary; finer ones become a solid line.
const MAX_HATCH_DASHES: f64 = 2_000.0;

fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// The dashes, between `t0` and `t1`, of an AutoCAD pattern that starts at 0 and repeats; dots
/// are spans of zero length.
fn dash_spans(t0: f64, t1: f64, pattern: &[f64]) -> Vec<(f64, f64)> {
    let period: f64 = pattern.iter().map(|v| v.abs()).sum();
    if !(period > 0.0) || (t1 - t0) / period > MAX_HATCH_DASHES {
        return vec![(t0, t1)];
    }
    let mut out = Vec::new();
    let mut pos = (t0 / period).floor() * period;
    while pos <= t1 {
        for &v in pattern {
            let end = pos + v.abs();
            let (a, b) = (pos.max(t0), end.min(t1));
            if v > 0.0 && a < b || v == 0.0 && (t0..=t1).contains(&pos) {
                out.push((a, b));
            }
            pos = end;
        }
    }
    out
}

/// Points along a polyline, its arcs flattened as in `segment_points`.
pub(crate) fn polyline_points(verts: &[Vertex], closed: bool) -> Vec<P> {
    let mut out: Vec<P> = verts.first().map(|v| v.0).into_iter().collect();
    for i in 0..segment_count(verts, closed) {
        out.extend(segment_points(verts, i).into_iter().skip(1).map(|(p, _)| p));
    }
    out
}

/// Points along a hatch boundary's elliptical arc edge, `major` being the major axis (from the
/// centre) and `ratio` minor / major. A clockwise edge's angles are measured clockwise.
pub(crate) fn arc_edge_points(centre: P, major: P, ratio: f64, start: f64, end: f64, ccw: bool) -> Vec<P> {
    let side = if ccw { ratio } else { -ratio };
    let (u, v) = (major, [-major[1] * side, major[0] * side]);
    let sweep = sweep(start, end);
    let steps = ((sweep / (TAU / 64.0)).ceil() as usize).max(2);
    (0..=steps)
        .map(|k| {
            let (s, c) = (start + sweep * k as f64 / steps as f64).sin_cos();
            [centre[0] + u[0] * c + v[0] * s, centre[1] + u[1] * c + v[1] * s]
        })
        .collect()
}

/// Canvas dash array (alternating on and off lengths, starting with on) for an AutoCAD linetype
/// pattern, or empty if it has no gaps. A leading gap moves to the end.
fn canvas_dash(pattern: impl IntoIterator<Item = f64>) -> Vec<f64> {
    let (mut out, mut lead): (Vec<f64>, f64) = (Vec::new(), 0.0);
    for v in pattern {
        let on = v >= 0.0;
        if out.is_empty() && !on {
            lead -= v;
        } else if !out.is_empty() && (out.len() % 2 == 1) == on {
            *out.last_mut().unwrap() += v.abs();
        } else {
            out.push(v.abs());
        }
    }
    if out.len() % 2 == 1 {
        out.push(0.0);
    }
    if let Some(gap) = out.last_mut() {
        *gap += lead;
    }
    if out.iter().skip(1).step_by(2).all(|&gap| gap <= 0.0) {
        out.clear();
    }
    out
}

/// `$INSUNITS` code to unit name, or "" if unitless.
pub(crate) fn units_name(code: i64) -> &'static str {
    match code {
        1 => "in",
        2 => "ft",
        3 => "mi",
        4 => "mm",
        5 => "cm",
        6 => "m",
        7 => "km",
        8 => "µin",
        9 => "mil",
        10 => "yd",
        13 => "µm",
        14 => "dm",
        _ => "",
    }
}

pub struct Layer {
    pub name: String,
    pub color: u32,
    /// False if the layer is off or frozen.
    pub visible: bool,
    /// Linetype name, or empty for continuous.
    pub linetype: String,
    /// Line weight in 1/100 mm, or -3 for the drawing's default.
    pub weight: i16,
}

fn layer(r: &Record) -> Layer {
    let index = r.i(62, 7);
    let frozen = r.i(70, 0) & 1 != 0;
    // A negative colour index means the layer is off.
    let name = r.get(2).unwrap_or("").trim().to_string();
    let linetype = r.get(6).unwrap_or("").trim().to_string();
    Layer { name, color: true_color(r).unwrap_or_else(|| color::aci(index.abs())), visible: index >= 0 && !frozen, linetype, weight: r.i(370, -3) as i16 }
}

fn true_color(r: &Record) -> Option<u32> {
    r.get(420).and_then(|v| v.trim().parse::<i64>().ok()).map(|v| v as u32 & 0xFF_FFFF)
}

/// An entity plus the VERTEX/ATTRIB records that follow it up to SEQEND.
struct Entity<'a> {
    rec: Record<'a>,
    children: Vec<Record<'a>>,
}

fn group<'a>(records: &[Record<'a>]) -> Vec<Entity<'a>> {
    let mut out: Vec<Entity> = Vec::new();
    for &rec in records {
        match rec.kind {
            "VERTEX" | "ATTRIB" => out.last_mut().into_iter().for_each(|e| e.children.push(rec)),
            "SEQEND" => {}
            _ => out.push(Entity { rec, children: Vec::new() }),
        }
    }
    out
}

struct Block<'a> {
    base: P,
    /// Handle of its block record (upper-case hex), which multileaders refer to it by.
    record: String,
    entities: Vec<Entity<'a>>,
}

fn parse_blocks<'a>(records: &[Record<'a>]) -> HashMap<String, Block<'a>> {
    let mut blocks = HashMap::new();
    let mut rest = records;
    while let Some(start) = rest.iter().position(|r| r.kind == "BLOCK") {
        let head = rest[start];
        let body = &rest[start + 1..];
        let end = body.iter().position(|r| r.kind == "ENDBLK").unwrap_or(body.len());
        blocks.insert(head.name(2), Block { base: head.point(10), record: head.name(330), entities: group(&body[..end]) });
        rest = &body[end..];
    }
    blocks
}

/// 2D affine transform in canvas order: x' = a·x + c·y + e, y' = b·x + d·y + f.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Affine([f64; 6]);

impl Affine {
    pub(crate) const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// From `[a, b, c, d, e, f]` in canvas (and XAML `m11, m12, m21, m22, dx, dy`) order.
    pub(crate) fn new(m: [f64; 6]) -> Affine {
        Affine(m)
    }

    pub(crate) fn translate([x, y]: P) -> Affine {
        Affine([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub(crate) fn rotate(angle: f64) -> Affine {
        let (s, c) = angle.sin_cos();
        Affine([c, s, -s, c, 0.0, 0.0])
    }

    pub(crate) fn scale(sx: f64, sy: f64) -> Affine {
        Affine([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    /// Object coordinate system of a 2D entity with extrusion Z component `z`. Only the common case
    /// of a flipped extrusion (0, 0, -1), produced by mirroring, is handled: it maps OCS (x, y) to world (-x, y).
    pub(crate) fn extrusion(z: f64) -> Affine {
        if z < 0.0 { Affine::scale(-1.0, 1.0) } else { Affine::IDENTITY }
    }

    fn ocs(r: &Record) -> Affine {
        Affine::extrusion(r.f(230, 1.0))
    }

    /// `self` applied after `o`.
    pub(crate) fn then(&self, o: &Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [oa, ob, oc, od, oe, of] = o.0;
        Affine([a * oa + c * ob, b * oa + d * ob, a * oc + c * od, b * oc + d * od, a * oe + c * of + e, b * oe + d * of + f])
    }

    pub(crate) fn apply(&self, [x, y]: P) -> P {
        let [a, b, c, d, e, f] = self.0;
        [a * x + c * y + e, b * x + d * y + f]
    }

    pub(crate) fn linear(&self, [x, y]: P) -> P {
        let [a, b, c, d, ..] = self.0;
        [a * x + c * y, b * x + d * y]
    }

    /// How much lengths grow, on average over directions: the square root of the area scale.
    pub(crate) fn length_scale(&self) -> f64 {
        let [a, b, c, d, ..] = self.0;
        (a * d - b * c).abs().sqrt()
    }
}

/// What an INSERT or DIMENSION passes on to its block's entities: its layer, which entities on
/// layer 0 take on, and its colour and linetype (index), for BYBLOCK.
#[derive(Clone, Copy)]
pub(crate) struct Parent<'a> {
    pub layer: &'a str,
    pub color: u32,
    pub linetype: u32,
    pub weight: u8,
}

/// Inherited state while expanding blocks.
pub(crate) struct Ctx<'a> {
    pub m: Affine,
    /// The enclosing INSERT or DIMENSION, if any.
    pub parent: Option<Parent<'a>>,
    pub depth: u32,
}

impl Ctx<'_> {
    pub(crate) const ROOT: Ctx<'static> = Ctx { m: Affine::IDENTITY, parent: None, depth: 0 };
}

/// What an entity passes on as a parent, and its pen. `index` is the ACI colour (0 BYBLOCK, 256 or
/// negative BYLAYER); `true_color` overrides it. `linetype` is the entity's linetype name (empty
/// for BYLAYER) and scale.
/// `weight` is the entity's line weight in 1/100 mm, or -1 BYLAYER, -2 BYBLOCK, -3 the drawing's default.
#[allow(clippy::too_many_arguments)]
pub(crate) fn style<'a>(w: &mut World, own: &'a str, true_color: Option<u32>, index: i64, linetype: (&str, f64), weight: i64, ctx: &Ctx<'a>) -> (Parent<'a>, Pen) {
    let name = match ctx.parent {
        Some(parent) if own == "0" => parent.layer,
        _ => own,
    };
    let layer = w.layer_id(name);
    let by_layer = &w.layers[layer as usize];
    let color = true_color.unwrap_or_else(|| match index {
        0 => ctx.parent.map_or(FOREGROUND, |p| p.color),
        256.. | ..0 => by_layer.color,
        index => color::aci(index),
    });
    let (own_linetype, scale) = linetype;
    let linetype = match own_linetype.trim().to_uppercase().as_str() {
        "" | "BYLAYER" => w.linetype_id(&by_layer.linetype),
        "BYBLOCK" => ctx.parent.map_or(0, |p| p.linetype),
        name => w.linetype_id(name),
    };
    let dash = w.dash(linetype, scale * ctx.m.length_scale());
    let weight = match weight {
        -1 => w.layers[layer as usize].weight.into(),
        own => own,
    };
    let weight = match weight {
        -2 => ctx.parent.map_or(w.default_weight, |p| p.weight),
        0..=211 => weight as u8,
        _ => w.default_weight,
    };
    (Parent { layer: name, color, linetype, weight }, Pen { color, layer, width: 0.0, dash, weight })
}

/// Block transforms for an INSERT at `at` (after `m` and the entity's OCS `ocs`): one per cell of
/// a MINSERT grid of `cols` × `rows`, `step` apart.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_cells(m: &Affine, ocs: Affine, at: P, rotation: f64, scale: P, base: P, (cols, rows): (i64, i64), step: P) -> Vec<Affine> {
    let placed = m.then(&ocs).then(&Affine::translate(at)).then(&Affine::rotate(rotation));
    let scaled = Affine::scale(scale[0], scale[1]).then(&Affine::translate([-base[0], -base[1]]));
    let (cols, rows) = (cols.clamp(1, 1000), rows.clamp(1, 1000));
    (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .map(|(row, col)| placed.then(&Affine::translate([col as f64 * step[0], row as f64 * step[1]])).then(&scaled))
        .collect()
}

/// Anchor, rotation and (horizontal, vertical) canvas alignment of a TEXT or ATTRIB, from its DXF
/// alignment codes (72: 0–5, 73/74: 0–3), first and second alignment points and rotation.
pub(crate) fn text_anchor(halign: i64, valign: i64, first: P, second: P, rotation: f64) -> (P, f64, (u8, u8)) {
    let (pos, rotation) = match halign {
        // Aligned and fit: the baseline runs from the first to the second point.
        3 | 5 => (first, (second[1] - first[1]).atan2(second[0] - first[0])),
        0 if valign == 0 => (first, rotation),
        _ => (second, rotation),
    };
    let align = match halign {
        1 => (1, valign),
        2 => (2, valign),
        4 => (1, 2),
        _ => (0, valign),
    };
    (pos, rotation, (align.0 as u8, align.1 as u8))
}

/// Canvas alignment of MTEXT attachment 1–9: top/middle/bottom rows of left/centre/right.
pub(crate) fn mtext_align(attach: i64) -> (u8, u8) {
    let attach = attach.clamp(1, 9) - 1;
    ((attach % 3) as u8, [3, 2, 1][attach as usize / 3])
}

/// DIMSTYLE group codes and the header variables with the same meaning.
const DIM_VARS: [(i32, &str); 13] = [
    (41, "$DIMASZ"),
    (140, "$DIMTXT"),
    (42, "$DIMEXO"),
    (44, "$DIMEXE"),
    (147, "$DIMGAP"),
    (77, "$DIMTAD"),
    (73, "$DIMTIH"),
    (144, "$DIMLFAC"),
    (271, "$DIMDEC"),
    (179, "$DIMADEC"),
    (78, "$DIMZIN"),
    (278, "$DIMDSEP"),
    (40, "$DIMSCALE"),
];

/// Slants text at `at` by `oblique` (radians, from upright towards its baseline at `rotation`), then
/// mirrors it along the baseline for TEXT generation flag 2 (backward) and across it for 4 (upside down).
pub(crate) fn text_shape(flags: i64, oblique: f64, at: P, rotation: f64) -> Affine {
    let flip = |bit: i64| if flags & bit != 0 { -1.0 } else { 1.0 };
    let turn = Affine::translate(at).then(&Affine::rotate(rotation));
    let back = Affine::rotate(-rotation).then(&Affine::translate([-at[0], -at[1]]));
    turn.then(&Affine::scale(flip(2), flip(4))).then(&Affine::new([1.0, 0.0, oblique.tan(), 1.0, 0.0, 0.0])).then(&back)
}

/// Unique edges of polyface `faces` (1-based vertex indices into `verts`, 0 for none; a negative
/// index hides the edge that starts there), as two-point paths.
pub(crate) fn polyface_edges(verts: &[P], faces: &[[i64; 4]]) -> Vec<Vec<P>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for face in faces {
        let idx: Vec<i64> = face.iter().copied().filter(|&i| i != 0).collect();
        for k in 0..idx.len() {
            let (a, b) = (idx[k], idx[(k + 1) % idx.len()]);
            let (Some(&p), Some(&q)) = (verts.get(a.unsigned_abs() as usize - 1), verts.get(b.unsigned_abs() as usize - 1)) else { continue };
            if a > 0 && seen.insert((a.abs().min(b.abs()), a.abs().max(b.abs()))) {
                out.push(vec![p, q]);
            }
        }
    }
    out
}

/// The lines of an `m` × `n` polygon mesh (vertices row by row), closed in M and N as flagged.
pub(crate) fn mesh_grid(verts: &[P], m: usize, n: usize, closed_m: bool, closed_n: bool) -> Vec<Vec<P>> {
    if m == 0 || n == 0 || verts.len() < m * n {
        return Vec::new();
    }
    let at = |i: usize, j: usize| verts[i * n + j];
    let rows = (0..m).map(|i| (0..n).chain(closed_n.then_some(0)).map(|j| at(i, j)).collect());
    let cols = (0..n).map(|j| (0..m).chain(closed_m.then_some(0)).map(|i| at(i, j)).collect());
    rows.chain(cols).collect()
}

/// A WIPEOUT's boundary in world coordinates: `origin`, pixel vectors `u` and `v`, size in pixels,
/// and clip points in pixel coordinates (from the top left), two of them for a rectangle.
pub(crate) fn wipeout_ring(origin: P, u: P, v: P, size: P, clip: &[P]) -> Vec<P> {
    let corner = [origin[0] + u[0] * 0.5 - v[0] * 0.5, origin[1] + u[1] * 0.5 - v[1] * 0.5];
    let world = |p: P| [corner[0] + u[0] * p[0] + v[0] * (size[1] - p[1]), corner[1] + u[1] * p[0] + v[1] * (size[1] - p[1])];
    let pixels = match clip {
        [a, b] => vec![*a, [b[0], a[1]], *b, [a[0], b[1]]],
        [] => vec![[-0.5, -0.5], [size[0] - 0.5, -0.5], [size[0] - 0.5, size[1] - 0.5], [-0.5, size[1] - 0.5]],
        _ => clip.to_vec(),
    };
    pixels.into_iter().map(world).collect()
}

/// The two colours of a gradient: its own, or for a one-colour gradient the first and its tint
/// (0 dark to 1 light) toward black or white.
pub(crate) fn gradient_colors(colors: &[u32], one_color: bool, tint: f64) -> Option<(u32, u32)> {
    let c1 = *colors.first()?;
    if !one_color && colors.len() > 1 {
        return Some((c1, colors[1]));
    }
    let (toward, k) = if tint >= 0.5 { (255.0, (tint - 0.5) * 2.0) } else { (0.0, (0.5 - tint) * 2.0) };
    let mix = |shift: u32| {
        let c = ((c1 >> shift) & 255) as f64;
        ((c + (toward - c) * k).round() as u32) << shift
    };
    Some((c1, mix(16) | mix(8) | mix(0)))
}

struct Builder<'a, 'b> {
    blocks: &'b HashMap<String, Block<'a>>,
    world: World<'a>,
    /// Dimension styles by upper-case name, and for dimensions whose style is not listed.
    dim_styles: HashMap<String, DimStyle>,
    dim_style: DimStyle,
    /// SAB data of ACIS entities (R2013+), by upper-case entity handle.
    acis: HashMap<String, Vec<u8>>,
}

impl<'a, 'b> Builder<'a, 'b> {
    fn entity(&mut self, e: &Entity<'a>, ctx: &Ctx<'a>) {
        let r = &e.rec;
        // Paper space and invisible entities are not drawn.
        if r.i(67, 0) == 1 || r.i(60, 0) == 1 {
            return;
        }
        let own = r.get(8).map_or("0", str::trim);
        let (parent, pen) = style(&mut self.world, own, true_color(r), r.i(62, 256), (r.get(6).unwrap_or(""), r.f(48, 1.0)), r.i(370, -1), ctx);
        let (m, w) = (ctx.m, &mut self.world);
        match r.kind {
            "LINE" => w.path(vec![r.point(10), r.point(11)], &m, pen),
            "POINT" => w.path(vec![r.point(10); 2], &m, pen),
            "CIRCLE" => w.circle_arc(r.point(10), r.f(40, 0.0), 0.0, TAU, &m.then(&Affine::ocs(r)), pen),
            "ARC" => {
                let (start, end) = (r.f(50, 0.0).to_radians(), r.f(51, 360.0).to_radians());
                w.circle_arc(r.point(10), r.f(40, 0.0), start, sweep(start, end), &m.then(&Affine::ocs(r)), pen);
            }
            "ELLIPSE" => {
                let ratio = r.f(40, 1.0) * r.f(230, 1.0).signum();
                w.ellipse(r.point(10), r.point(11), ratio, r.f(41, 0.0), r.f(42, TAU), &m, pen);
            }
            "LWPOLYLINE" => {
                // Constant width (43) applies to vertices without their own widths (40, 41).
                let constant = r.f(43, 0.0);
                let mut verts: Vec<Vertex> = Vec::new();
                for p in r.pairs {
                    let v = || p.value.trim().parse().unwrap_or(0.0);
                    match (p.code, verts.last_mut()) {
                        (10, _) => verts.push(([v(), 0.0], 0.0, [constant; 2])),
                        (20, Some(last)) => last.0[1] = v(),
                        (40, Some(last)) => last.2[0] = v(),
                        (41, Some(last)) => last.2[1] = v(),
                        (42, Some(last)) => last.1 = v(),
                        _ => {}
                    }
                }
                w.polyline(&verts, r.i(70, 0) & 1 != 0, &m.then(&Affine::ocs(r)), pen);
            }
            "POLYLINE" => {
                let flags = r.i(70, 0);
                let vertices = e.children.iter().filter(|v| v.kind == "VERTEX");
                // Polyface mesh: vertices (flag 64) and faces (128 alone) listing up to four 1-based vertex indices.
                if flags & 64 != 0 {
                    let verts: Vec<P> = vertices.clone().filter(|v| v.i(70, 0) & 64 != 0).map(|v| v.point(10)).collect();
                    let faces: Vec<[i64; 4]> = vertices.filter(|v| v.i(70, 0) & 192 == 128).map(|v| [71, 72, 73, 74].map(|c| v.i(c, 0))).collect();
                    polyface_edges(&verts, &faces).into_iter().for_each(|edge| w.path(edge, &m, pen));
                    return;
                }
                // Polygon mesh: M × N vertices (71, 72), closed in M (flag 1) and N (32).
                if flags & 16 != 0 {
                    let verts: Vec<P> = vertices.map(|v| v.point(10)).collect();
                    let (rows, cols) = (r.i(71, 0).max(0) as usize, r.i(72, 0).max(0) as usize);
                    mesh_grid(&verts, rows, cols, flags & 1 != 0, flags & 32 != 0).into_iter().for_each(|line| w.path(line, &m, pen));
                    return;
                }
                // Spline frame control points (vertex flag 16) are not part of the curve.
                // Vertices without their own widths take the polyline's defaults (40, 41).
                let widths = |v: &Record| [v.f(40, r.f(40, 0.0)), v.f(41, r.f(41, 0.0))];
                let verts: Vec<_> = e.children.iter().filter(|v| v.kind == "VERTEX" && v.i(70, 0) & 16 == 0).map(|v| (v.point(10), v.f(42, 0.0), widths(v))).collect();
                let ocs = if flags & 8 != 0 { Affine::IDENTITY } else { Affine::ocs(r) };
                w.polyline(&verts, flags & 1 != 0, &m.then(&ocs), pen);
            }
            // SOLID and TRACE list their corners in zig-zag order; a triangle repeats the third.
            "SOLID" | "TRACE" => {
                let c = r.point(12);
                let d = if r.get(13).is_some() { r.point(13) } else { c };
                w.solid([r.point(10), r.point(11), c, d], &m.then(&Affine::ocs(r)), pen);
            }
            "3DFACE" => {
                let pts = [10, 11, 12, 13, 10].map(|code| r.point(code));
                w.path(pts.to_vec(), &m, pen);
            }
            "SPLINE" => w.path(spline(r), &m, pen),
            // A helix is its spline, which comes before the AcDbHelix groups (whose 10 is the axis base point).
            "HELIX" => {
                let end = r.pairs.iter().position(|p| p.code == 100 && p.value.trim() == "AcDbHelix").unwrap_or(r.pairs.len());
                w.path(spline(&Record { kind: r.kind, pairs: &r.pairs[..end] }), &m, pen);
            }
            "MESH" => {
                let (verts, faces) = subd_mesh(r);
                polyface_edges(&verts, &faces).into_iter().for_each(|edge| w.path(edge, &m, pen));
            }
            "MLINE" => mline(r).into_iter().for_each(|line| w.path(line, &m, pen)),
            "WIPEOUT" => {
                let clip: Vec<P> = r.pairs.iter().filter(|p| p.code == 14).zip(r.pairs.iter().filter(|p| p.code == 24)).map(|(x, y)| [x.value.trim().parse().unwrap_or(0.0), y.value.trim().parse().unwrap_or(0.0)]).collect();
                w.wipeout(wipeout_ring(r.point(10), r.point(11), r.point(12), r.point(13), &clip), &m, pen.layer);
            }
            "TOLERANCE" => {
                let dir = if r.get(11).is_some() { r.point(11) } else { [1.0, 0.0] };
                let style = self.dim_styles.get(&r.name(3)).unwrap_or(&self.dim_style).scaled();
                dim::tolerance(w, r.point(10), dir, r.get(1).unwrap_or(""), &style, &m, pen);
            }
            // A table's graphics are an anonymous block, placed like an INSERT along its direction (11).
            "ACAD_TABLE" => {
                let dir = if r.get(11).is_some() { r.point(11) } else { [1.0, 0.0] };
                let at = m.then(&Affine::translate(r.point(10))).then(&Affine::rotate(dir[1].atan2(dir[0])));
                self.expand(&r.name(2), at, parent, ctx);
            }
            "RAY" | "XLINE" => w.ray(r.point(10), r.point(11), r.kind == "XLINE", &m, pen),
            "LEADER" => {
                let points = r.pairs.iter().filter(|p| p.code == 10).zip(r.pairs.iter().filter(|p| p.code == 20));
                let points = points.map(|(x, y)| [x.value.trim().parse().unwrap_or(0.0), y.value.trim().parse().unwrap_or(0.0)]).collect();
                // Vertices are in world coordinates. Arrowhead flag 71 (default on); path type 72: 1 is a spline.
                let size = if r.i(71, 1) == 0 { 0.0 } else { self.dim_styles.get(&r.name(3)).unwrap_or(&self.dim_style).scaled().arrow };
                w.leader(points, r.i(72, 0) == 1, size, &m, pen);
            }
            "HATCH" => {
                let (rings, pattern) = hatch(r);
                let ocs = m.then(&Affine::ocs(r));
                match gradient(r) {
                    Some((name, angle, c1, c2)) => w.gradient(rings, &name, angle, c1, c2, &ocs, pen),
                    None => w.hatch(rings, pattern.as_deref(), &ocs, pen),
                }
            }
            // An ATTDEF shows its tag outside blocks; inside one, only a constant one (flag 2) shows, its value.
            "TEXT" | "ATTRIB" | "ATTDEF" => {
                let value = match r.kind {
                    "ATTDEF" if ctx.depth == 0 => r.get(2),
                    "ATTDEF" if r.i(70, 0) & 2 != 0 => r.get(1),
                    "ATTDEF" => return,
                    _ => r.get(1),
                };
                // Attribute flag 1: invisible. Attributes keep their vertical alignment in 74; TEXT in 73.
                if r.kind != "TEXT" && r.i(70, 0) & 1 != 0 {
                    return;
                }
                let valign = r.i(if r.kind == "TEXT" { 73 } else { 74 }, 0);
                let (pos, rotation, align) = text_anchor(r.i(72, 0), valign, r.point(10), r.point(11), r.f(50, 0.0).to_radians());
                let string = parse::text_rich(value.unwrap_or(""));
                let m = m.then(&Affine::ocs(r)).then(&text_shape(r.i(71, 0), r.f(51, 0.0).to_radians(), pos, rotation));
                w.push_text(pos, rotation, r.f(40, 1.0), r.f(41, 1.0), align, string, &m, pen);
            }
            "MTEXT" => {
                // Long text is split into 250-character chunks in code 3, followed by the tail in code 1.
                let raw: String = r.pairs.iter().filter(|p| p.code == 3).chain(r.pairs.iter().filter(|p| p.code == 1)).map(|p| p.value).collect();
                let rotation = match r.get(11) {
                    Some(_) => {
                        let d = r.point(11);
                        d[1].atan2(d[0])
                    }
                    None => r.f(50, 0.0),
                };
                let align = mtext_align(r.i(71, 1));
                let start = w.text_count();
                w.mtext(r.point(10), rotation, r.f(40, 1.0), r.f(41, 0.0), align, parse::mtext_rich(&raw, r.f(40, 1.0)), &m, pen);
                // Background mask (90): flag 1 in its own colour (63, 421), 2 in the paper's; 45 sizes it in text heights.
                let flags = r.i(90, 0);
                if flags & 3 != 0 {
                    let color = if flags & 2 != 0 { BACKGROUND } else { true_color(r).unwrap_or_else(|| color::aci(r.i(63, 7))) };
                    w.mask_texts(start, color, (r.f(45, 1.5) - 1.0) * r.f(40, 1.0), &m);
                }
            }
            "INSERT" => {
                let Some(block) = self.blocks.get(&r.name(2)) else { return };
                let grid = (r.i(70, 1), r.i(71, 1));
                let scale = [r.f(41, 1.0), r.f(42, 1.0)];
                let step = [r.f(44, 0.0), r.f(45, 0.0)];
                for cell in insert_cells(&m, Affine::ocs(r), r.point(10), r.f(50, 0.0).to_radians(), scale, block.base, grid, step) {
                    self.expand(&r.name(2), cell, parent, ctx);
                }
                for attrib in &e.children {
                    // Attribute flag 1: invisible.
                    if attrib.kind == "ATTRIB" && attrib.i(70, 0) & 1 == 0 {
                        self.entity(&Entity { rec: *attrib, children: Vec::new() }, ctx);
                    }
                }
            }
            "MULTILEADER" | "MLEADER" => {
                let (ml, line_color, text_color) = multileader(r);
                let recolor = |c: Option<u32>| Pen { color: c.unwrap_or(pen.color), ..pen };
                self.multileader(&ml, m, parent, recolor(line_color), recolor(text_color), ctx);
            }
            // A dimension's lines, arrows and text are in an anonymous block, already in world
            // coordinates. Without one it is drawn from its definition points.
            "DIMENSION" | "ARC_DIMENSION" | "LARGE_RADIAL_DIMENSION" => {
                let name = r.name(2);
                if self.blocks.get(&name).is_some_and(|b| !b.entities.is_empty()) {
                    return self.expand(&name, m, parent, ctx);
                }
                let Some(d) = dimension(r) else { return };
                let text = DimText { user: r.get(1).unwrap_or(""), at: r.get(11).map(|_| r.point(11)), rotation: r.get(53).map(|_| r.f(53, 0.0).to_radians()).filter(|&a| a != 0.0) };
                let style = self.dim_styles.get(&r.name(3)).unwrap_or(&self.dim_style).scaled();
                dim::draw(w, &d, &text, &style, &m, pen);
            }
            // ACIS bodies: SAT text (encoded) before R2013, then SAB data in the ACDSDATA section.
            "REGION" | "3DSOLID" | "BODY" => {
                let sat: String = r.pairs.iter().filter(|p| matches!(p.code, 1 | 3)).map(|p| if p.code == 1 { format!("{}\n", p.value) } else { p.value.to_string() }).collect();
                let sab = self.acis.get(&r.name(5)).map_or(&[][..], Vec::as_slice);
                for edge in acis::read(&AcisData::decode_sat(&sat), sab).as_ref().map(acis::edges).unwrap_or_default() {
                    w.path(edge, &m, pen);
                }
            }
            kind => w.skip(kind),
        }
    }

    /// Draws a multileader: its lines and text, arrowheads (blocks or the default) and block content.
    fn multileader(&mut self, ml: &MultiLeader, m: Affine, parent: Parent<'a>, line: Pen, text: Pen, ctx: &Ctx<'a>) {
        for (at, arrow) in self.world.multileader(ml, &m, line, text) {
            match arrow.and_then(|h| self.block_name(&h)) {
                Some(name) => self.expand(&name, at, Parent { color: line.color, ..parent }, ctx),
                None => self.world.arrowhead(&at, line),
            }
        }
        let Some((handle, at, scale, rotation)) = &ml.block else { return };
        let Some(name) = self.block_name(handle) else { return };
        let base = self.blocks[&name].base;
        for cell in insert_cells(&m, Affine::IDENTITY, *at, *rotation, *scale, base, (1, 1), [0.0; 2]) {
            self.expand(&name, cell, parent, ctx);
        }
    }

    /// Name of the block whose record has handle `hex`.
    fn block_name(&self, hex: &str) -> Option<String> {
        self.blocks.iter().find(|(_, b)| b.record == hex).map(|(name, _)| name.clone())
    }

    /// Draws block `name` with transform `m`, as the child of an INSERT or DIMENSION.
    fn expand(&mut self, name: &str, m: Affine, parent: Parent<'a>, ctx: &Ctx<'a>) {
        if ctx.depth >= MAX_DEPTH {
            return;
        }
        let Some(block) = self.blocks.get(name) else { return };
        let inner = Ctx { m, parent: Some(parent), depth: ctx.depth + 1 };
        for e in &block.entities {
            self.entity(e, &inner);
        }
    }
}

/// The elements of an LTYPE, in file order; None if it has text, or shapes that are not built in
/// (see `shapes`), which are drawn solid instead.
fn linetype_elements(r: &Record, shape_files: &HashMap<String, &str>) -> Option<Vec<LinetypeElement>> {
    // Each element with its shape's number and file, looked up once all groups are read.
    let mut out: Vec<(LinetypeElement, i64, &str)> = Vec::new();
    for p in r.pairs {
        let v = || p.value.trim().parse::<f64>().unwrap_or(0.0);
        if p.code == 49 {
            out.push((LinetypeElement::plain(v()), 0, ""));
            continue;
        }
        let Some((e, number, file)) = out.last_mut() else { continue };
        match (p.code, &mut e.shape) {
            // Element flags: 1 absolute rotation, 2 text, 4 shape.
            (74, _) => {
                let flags = v() as i64;
                if flags & 2 != 0 {
                    return None;
                }
                if flags & 4 != 0 {
                    e.shape = Some(LinetypeShape { strokes: Vec::new(), scale: 1.0, rotation: 0.0, absolute: flags & 1 != 0, offset: [0.0; 2] });
                }
            }
            (75, _) => *number = v() as i64,
            (340, _) => *file = shape_files.get(&p.value.trim().to_uppercase()).copied().unwrap_or(""),
            (46, Some(s)) => s.scale = v(),
            (50, Some(s)) => s.rotation = v().to_radians(),
            (44, Some(s)) => s.offset[0] = v(),
            (45, Some(s)) => s.offset[1] = v(),
            _ => {}
        }
    }
    out.into_iter()
        .map(|(mut e, number, file)| {
            if let Some(s) = &mut e.shape {
                s.strokes = shapes::strokes(file, number)?;
            }
            Some(e)
        })
        .collect()
}

/// SAB data of the ACDSDATA section's ASM_Data records, by upper-case owner entity handle.
fn acds_data(records: &[Record]) -> HashMap<String, Vec<u8>> {
    let hex = |s: &str| (0..s.len() / 2).filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect::<Vec<u8>>();
    records
        .iter()
        .filter(|r| r.kind == "ACDSRECORD" && r.pairs.iter().any(|p| p.code == 2 && p.value.trim() == "ASM_Data"))
        .map(|r| (r.name(320), r.pairs.iter().filter(|p| p.code == 310).flat_map(|p| hex(p.value.trim())).collect()))
        .collect()
}

/// Reads groups in file order, as the HATCH boundary and pattern data needs.
struct Cursor<'a, 'b> {
    pairs: &'b [Pair<'a>],
    i: usize,
}

impl Cursor<'_, '_> {
    fn peek(&self, ahead: usize) -> Option<i32> {
        self.pairs.get(self.i + ahead).map(|p| p.code)
    }

    /// The next group's value if it has `code`.
    fn take(&mut self, code: i32) -> Option<f64> {
        let p = self.pairs.get(self.i).filter(|p| p.code == code)?;
        self.i += 1;
        Some(p.value.trim().parse().unwrap_or(0.0))
    }

    fn f(&mut self, code: i32) -> f64 {
        self.take(code).unwrap_or(0.0)
    }

    /// A count, at most the number of groups left.
    fn n(&mut self, code: i32) -> usize {
        (self.f(code).max(0.0) as usize).min(self.pairs.len() - self.i)
    }

    fn xy(&mut self, x: i32, y: i32) -> P {
        [self.f(x), self.f(y)]
    }

    /// Moves past the next group with `code` and returns its value.
    fn seek(&mut self, code: i32) -> Option<f64> {
        self.i += self.pairs[self.i..].iter().position(|p| p.code == code)?;
        self.take(code)
    }
}

/// A dimension's geometry from its definition points; None for an unknown type.
fn dimension(r: &Record) -> Option<Dim> {
    let p = |code| r.point(code);
    Some(match r.kind {
        "ARC_DIMENSION" => Dim::ArcLength { centre: p(15), a: p(13), b: p(14), arc: p(10) },
        "LARGE_RADIAL_DIMENSION" => Dim::Jogged { centre: p(10), chord: p(13), shown_centre: p(14), jog: p(15) },
        // Dimension type, the low bits of 70; 64 marks an X ordinate.
        _ => match r.i(70, 0) & 7 {
            0 => Dim::Linear { ext1: p(13), ext2: p(14), line: p(10), angle: Some(r.f(50, 0.0).to_radians()) },
            1 => Dim::Linear { ext1: p(13), ext2: p(14), line: p(10), angle: None },
            2 => Dim::Angular2 { l1: [p(13), p(14)], l2: [p(15), p(10)], arc: p(16) },
            3 => Dim::Diameter { a: p(15), b: p(10) },
            4 => Dim::Radius { centre: p(10), on_curve: p(15) },
            5 => Dim::Angular3 { vertex: p(15), a: p(13), b: p(14), arc: p(10) },
            6 => Dim::Ordinate { origin: p(10), feature: p(13), leader: p(14), x: r.i(70, 0) & 64 != 0 },
            _ => return None,
        },
    })
}

/// A HATCH's gradient (450 = 1): its name, angle and two colours.
fn gradient(r: &Record) -> Option<(String, f64, u32, u32)> {
    if r.i(450, 0) != 1 {
        return None;
    }
    let start = r.pairs.iter().position(|p| p.code == 450)?;
    // Each colour starts with a 463 group, then an ACI (63) and usually a true colour (421).
    let mut colors: Vec<u32> = Vec::new();
    for p in &r.pairs[start..] {
        let v = p.value.trim();
        match p.code {
            463 => colors.push(FOREGROUND),
            63 => colors.last_mut().into_iter().for_each(|c| *c = color::aci(v.parse().unwrap_or(7))),
            421 => colors.last_mut().into_iter().for_each(|c| *c = v.parse::<i64>().map_or(*c, |rgb| rgb as u32 & 0xFF_FFFF)),
            _ => {}
        }
    }
    let after = Record { kind: r.kind, pairs: &r.pairs[start..] };
    let (c1, c2) = gradient_colors(&colors, after.i(452, 0) != 0, after.f(462, 0.0))?;
    Some((after.get(470).unwrap_or("LINEAR").trim().to_string(), after.f(460, 0.0), c1, c2))
}

/// A MESH's level-0 vertices and faces (as 1-based indices, for `polyface_edges`; faces of more than four sides are split into fans).
fn subd_mesh(r: &Record) -> (Vec<P>, Vec<[i64; 4]>) {
    let mut c = Cursor { pairs: r.pairs, i: 0 };
    let n = c.seek(92).map_or(0, |n| (n.max(0.0) as usize).min(r.pairs.len()));
    let verts: Vec<P> = (0..n)
        .map(|_| {
            let p = c.xy(10, 20);
            c.take(30);
            p
        })
        .collect();
    let size = c.seek(93).map_or(0, |n| (n.max(0.0) as usize).min(r.pairs.len()));
    let list: Vec<i64> = (0..size).map(|_| c.f(90) as i64).collect();
    let mut faces = Vec::new();
    let mut rest = &list[..];
    while let [count, tail @ ..] = rest {
        let count = (*count).max(0) as usize;
        let face: Vec<i64> = tail.iter().take(count).map(|i| i + 1).collect();
        for k in 1..face.len().saturating_sub(1) {
            // Fan triangles; their inner edges are hidden (negative start index).
            let (a, b, c) = (face[0], face[k], face[k + 1]);
            let first = if k == 1 { a } else { -a };
            let last = if k + 2 == face.len() { c } else { -c };
            faces.push([first, b, last, 0]);
        }
        rest = tail.get(count..).unwrap_or(&[]);
    }
    (verts, faces)
}

/// An MLINE's element lines: each vertex (11) moved along its miter (13) by the element's first parameter (41).
fn mline(r: &Record) -> Vec<Vec<P>> {
    let mut c = Cursor { pairs: r.pairs, i: 0 };
    let (count, elements) = (r.i(72, 0).max(0) as usize, r.i(73, 0).clamp(0, 64) as usize);
    let mut lines = vec![Vec::new(); elements];
    for _ in 0..count.min(r.pairs.len()) {
        let Some(x) = c.seek(11) else { break };
        let at = [x, c.f(21)];
        c.take(31);
        c.xy(12, 22);
        c.take(32);
        let miter = c.xy(13, 23);
        c.take(33);
        for line in &mut lines {
            let params: Vec<f64> = (0..c.n(74)).map(|_| c.f(41)).collect();
            for _ in 0..c.n(75) {
                c.take(42);
            }
            let t = params.first().copied().unwrap_or(0.0);
            line.push([at[0] + miter[0] * t, at[1] + miter[1] * t]);
        }
    }
    // Flag 2: closed.
    if r.i(71, 0) & 2 != 0 {
        lines.iter_mut().for_each(|l| l.extend(l.first().copied()));
    }
    lines
}

/// A HATCH's boundary rings and pattern lines (None for a solid fill).
fn hatch(r: &Record) -> (Vec<Vec<P>>, Option<Vec<HatchLine>>) {
    let mut c = Cursor { pairs: r.pairs, i: 0 };
    let mut rings = Vec::new();
    for _ in 0..c.seek(91).map_or(0, |n| (n.max(0.0) as usize).min(r.pairs.len())) {
        let mut ring = Vec::new();
        if c.f(92) as i64 & 2 != 0 {
            let (bulges, closed) = (c.f(72) != 0.0, c.f(73) != 0.0);
            let verts: Vec<Vertex> = (0..c.n(93)).map(|_| (c.xy(10, 20), if bulges { c.f(42) } else { 0.0 }, [0.0; 2])).collect();
            ring = polyline_points(&verts, closed);
        } else {
            for _ in 0..c.n(93) {
                match c.f(72) as i64 {
                    1 => ring.extend([c.xy(10, 20), c.xy(11, 21)]),
                    2 => {
                        let (centre, radius) = (c.xy(10, 20), c.f(40));
                        let (start, end) = (c.f(50).to_radians(), c.f(51).to_radians());
                        ring.extend(arc_edge_points(centre, [radius, 0.0], 1.0, start, end, c.f(73) != 0.0));
                    }
                    3 => {
                        let (centre, major, ratio) = (c.xy(10, 20), c.xy(11, 21), c.f(40));
                        let (start, end) = (c.f(50).to_radians(), c.f(51).to_radians());
                        ring.extend(arc_edge_points(centre, major, ratio, start, end, c.f(73) != 0.0));
                    }
                    4 => {
                        let (degree, rational) = (c.f(94) as usize, c.f(73) != 0.0);
                        c.take(74);
                        let (knot_count, ctrl_count) = (c.n(95), c.n(96));
                        let knots: Vec<f64> = (0..knot_count).map(|_| c.f(40)).collect();
                        let (mut ctrl, mut weights) = (Vec::new(), Vec::new());
                        for _ in 0..ctrl_count {
                            ctrl.push(c.xy(10, 20));
                            weights.extend(rational.then(|| c.f(42)));
                        }
                        // Fit data (R2010+) starts with a 97 count, as the path's source objects do after it.
                        let mut fit = Vec::new();
                        if c.peek(0) == Some(97) && matches!(c.peek(1), Some(11 | 97)) {
                            fit = (0..c.n(97)).map(|_| c.xy(11, 21)).collect();
                            for code in [12, 22, 13, 23] {
                                c.take(code);
                            }
                        }
                        ring.extend(spline_points(degree, &ctrl, &knots, &weights, &fit));
                    }
                    // Unknown edge data cannot be stepped over.
                    _ => break,
                }
            }
        }
        // Handles of the source boundary objects.
        for _ in 0..c.n(97) {
            c.take(330);
        }
        rings.push(ring);
    }
    if r.i(70, 0) & 1 != 0 || r.i(450, 0) == 1 {
        return (rings, None);
    }
    let count = c.seek(78).map_or(0, |n| (n.max(0.0) as usize).min(r.pairs.len()));
    let lines = (0..count)
        .map(|_| {
            let angle = c.f(53).to_radians();
            let (base, offset) = (c.xy(43, 44), c.xy(45, 46));
            HatchLine { angle, base, offset, dashes: (0..c.n(79)).map(|_| c.f(49)).collect() }
        })
        .collect();
    (rings, Some(lines))
}

/// A MULTILEADER's leaders and content from its CONTEXT_DATA{} section, with its leader line
/// and text colours if they are not BYLAYER or BYBLOCK.
fn multileader(r: &Record) -> (MultiLeader, Option<u32>, Option<u32>) {
    let mut ml = MultiLeader::default();
    let (mut text, mut has_text, mut block) = (LeaderText { value: String::new(), at: [0.0; 2], angle: 0.0, height: 0.0, column: 0 }, false, None);
    let (mut line_color, mut text_color) = (None, None);
    // 0: entity, 1: CONTEXT_DATA{, 2: LEADER{, 3: LEADER_LINE{.
    let mut depth = 0;
    let mut direction = [1.0, 0.0];
    for p in r.pairs {
        let (value, v) = (p.value.trim(), || p.value.trim().parse::<f64>().unwrap_or(0.0));
        let root = ml.roots.last_mut();
        match (depth, p.code) {
            (_, 300) if value == "CONTEXT_DATA{" => depth = 1,
            (1, 302) if value == "LEADER{" => {
                depth = 2;
                ml.roots.push(LeaderRoot::default());
            }
            (2, 304) if value == "LEADER_LINE{" => {
                depth = 3;
                root.into_iter().for_each(|r| r.lines.push(LeaderLine::default()));
            }
            (3, 305) | (2, 303) | (1, 301) if value == "}" => depth -= 1,
            (1, 41) => text.height = v(),
            (1, 140) => ml.arrow_size = v(),
            (1, 290) => has_text = v() != 0.0,
            (1, 304) => text.value = p.value.to_string(),
            (1, 12) => text.at[0] = v(),
            (1, 22) => text.at[1] = v(),
            (1, 13) => direction[0] = v(),
            (1, 23) => direction[1] = v(),
            (1, 90) => text_color = cm_color(v() as i64),
            (1, 171) => text.column = (v() as i64 - 1).clamp(0, 2) as u8,
            (1, 296) if v() != 0.0 => block = Some((String::new(), [0.0; 2], [1.0; 2], 0.0)),
            (1, 341) => block.iter_mut().for_each(|b| b.0 = value.to_uppercase()),
            (1, 15) => block.iter_mut().for_each(|b| b.1[0] = v()),
            (1, 25) => block.iter_mut().for_each(|b| b.1[1] = v()),
            (1, 16) => block.iter_mut().for_each(|b| b.2[0] = v()),
            (1, 26) => block.iter_mut().for_each(|b| b.2[1] = v()),
            (1, 46) => block.iter_mut().for_each(|b| b.3 = v()),
            (2, code @ (10 | 20 | 11 | 21 | 40)) => {
                let Some(r) = root else { continue };
                match code {
                    10 => r.connection[0] = v(),
                    20 => r.connection[1] = v(),
                    11 => r.direction[0] = v(),
                    21 => r.direction[1] = v(),
                    _ => r.landing = v(),
                }
            }
            (3, code @ (10 | 20 | 341)) => {
                let Some(line) = root.and_then(|r| r.lines.last_mut()) else { continue };
                match code {
                    10 => line.points.push([v(), 0.0]),
                    20 => line.points.last_mut().into_iter().for_each(|q| q[1] = v()),
                    _ => line.arrow = Some(value.to_uppercase()).filter(|h| h != "0"),
                }
            }
            (0, 91) => line_color = cm_color(v() as i64),
            (0, 291) => ml.dogleg = v() != 0.0,
            (0, 342) => ml.arrow = Some(value.to_uppercase()).filter(|h| h != "0"),
            _ => {}
        }
    }
    text.angle = direction[1].atan2(direction[0]);
    ml.text = has_text.then_some(text);
    ml.block = block.filter(|b| !b.0.is_empty());
    (ml, line_color, text_color)
}

/// An AcCmColor value (as MULTILEADER stores colours) as RGB, or None for BYLAYER or BYBLOCK.
fn cm_color(v: i64) -> Option<u32> {
    match (v >> 24) & 0xFF {
        0xC2 => Some(v as u32 & 0xFF_FFFF),
        0xC3 if (1..=255).contains(&(v & 0xFF)) => Some(color::aci(v & 0xFF)),
        _ => None,
    }
}

/// Flattens a SPLINE from its group codes.
fn spline(r: &Record) -> Vec<P> {
    let collect = |code: i32| -> Vec<f64> { r.pairs.iter().filter(|p| p.code == code).map(|p| p.value.trim().parse().unwrap_or(0.0)).collect() };
    let points = |x: i32, y: i32| -> Vec<P> { collect(x).into_iter().zip(collect(y)).map(|(x, y)| [x, y]).collect() };
    spline_points(r.i(71, 3).max(1) as usize, &points(10, 20), &collect(40), &collect(41), &points(11, 21))
}

/// Flattens a (rational) B-spline on its control points, or a curve through its fit points if it has none.
pub(crate) fn spline_points(degree: usize, ctrl: &[P], knots: &[f64], weights: &[f64], fit: &[P]) -> Vec<P> {
    if ctrl.is_empty() {
        return through(fit);
    }
    let degree = degree.max(1);
    let n = ctrl.len();
    if n <= degree || knots.len() != n + degree + 1 {
        return ctrl.to_vec();
    }
    let weights = if weights.len() == n { weights.to_vec() } else { vec![1.0; n] };
    // Homogeneous control points (x·w, y·w, w).
    let cw: Vec<[f64; 3]> = ctrl.iter().zip(&weights).map(|(p, &w)| [p[0] * w, p[1] * w, w]).collect();
    let de_boor = |span: usize, t: f64| -> P {
        let mut d: Vec<[f64; 3]> = (0..=degree).map(|j| cw[j + span - degree]).collect();
        for level in 1..=degree {
            for j in (level..=degree).rev() {
                let i = j + span - degree;
                let denom = knots[i + 1 + degree - level] - knots[i];
                let alpha = if denom == 0.0 { 0.0 } else { (t - knots[i]) / denom };
                for k in 0..3 {
                    d[j][k] = (1.0 - alpha) * d[j - 1][k] + alpha * d[j][k];
                }
            }
        }
        let [x, y, w] = d[degree];
        if w == 0.0 { [x, y] } else { [x / w, y / w] }
    };
    let mut out = Vec::new();
    let mut last = None;
    for span in degree..n {
        let (a, b) = (knots[span], knots[span + 1]);
        if b <= a {
            continue;
        }
        out.extend((0..SPLINE_STEPS).map(|s| de_boor(span, a + (b - a) * s as f64 / SPLINE_STEPS as f64)));
        last = Some((span, b));
    }
    out.extend(last.map(|(span, b)| de_boor(span, b)));
    out
}

/// The C2 cubic through `points`, parameterised by chord length with natural ends, as AutoCAD
/// fits a spline with no end tangents.
fn through(points: &[P]) -> Vec<P> {
    // Repeated points have no chord to parameterise by.
    let mut points = points.to_vec();
    points.dedup();
    let n = points.len();
    if n < 3 {
        return points;
    }
    let h: Vec<f64> = points.windows(2).map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1])).collect();
    // Tangents (dP/dt) at the points, from the tridiagonal system of C2 continuity (Thomas algorithm).
    let (mut b, mut c, mut d) = (vec![0.0; n], vec![0.0; n], vec![[0.0; 2]; n]);
    let slope = |i: usize, k: usize| (points[i + 1][k] - points[i][k]) / h[i];
    for k in 0..2 {
        d[0][k] = 3.0 * slope(0, k);
        d[n - 1][k] = 3.0 * slope(n - 2, k);
        for i in 1..n - 1 {
            d[i][k] = 3.0 * (h[i] * slope(i - 1, k) + h[i - 1] * slope(i, k));
        }
    }
    let a = |i: usize| if i == n - 1 { 1.0 } else { h[i] };
    (b[0], c[0]) = (2.0, 1.0);
    for i in 1..n {
        let (bi, ci) = if i == n - 1 { (2.0, 0.0) } else { (2.0 * (h[i - 1] + h[i]), h[i - 1]) };
        let m = a(i) / b[i - 1];
        b[i] = bi - m * c[i - 1];
        c[i] = ci;
        d[i] = [d[i][0] - m * d[i - 1][0], d[i][1] - m * d[i - 1][1]];
    }
    let mut tan = vec![[0.0; 2]; n];
    tan[n - 1] = [d[n - 1][0] / b[n - 1], d[n - 1][1] / b[n - 1]];
    for i in (0..n - 1).rev() {
        tan[i] = [(d[i][0] - c[i] * tan[i + 1][0]) / b[i], (d[i][1] - c[i] * tan[i + 1][1]) / b[i]];
    }
    let mut out = Vec::new();
    for i in 0..n - 1 {
        out.extend((0..SPLINE_STEPS).map(|s| {
            let t = s as f64 / SPLINE_STEPS as f64;
            let (t2, t3) = (t * t, t * t * t);
            let (h00, h10, h01, h11) = (2.0 * t3 - 3.0 * t2 + 1.0, t3 - 2.0 * t2 + t, 3.0 * t2 - 2.0 * t3, t3 - t2);
            let c = |k: usize| h00 * points[i][k] + h[i] * (h10 * tan[i][k] + h11 * tan[i + 1][k]) + h01 * points[i + 1][k];
            [c(0), c(1)]
        }));
    }
    out.push(points[n - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a DXF from `(code, value)` pairs.
    fn dxf(groups: &[(i32, &str)]) -> Vec<u8> {
        groups.iter().map(|(c, v)| format!("{c}\n{v}\n")).collect::<String>().into_bytes()
    }

    fn entities(body: &[(i32, &str)]) -> Vec<u8> {
        let mut g = vec![(0, "SECTION"), (2, "ENTITIES")];
        g.extend_from_slice(body);
        g.extend([(0, "ENDSEC"), (0, "EOF")]);
        dxf(&g)
    }

    fn close(a: P, b: P) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn line_extents_and_flip() {
        let d = decode(&entities(&[(0, "LINE"), (8, "0"), (10, "10"), (20, "5"), (11, "30"), (21, "25")])).unwrap();
        assert_eq!((d.width, d.height, d.origin), (20.0, 20.0, [10.0, 25.0]));
        assert!(close(d.paths[0].points[0], [0.0, 20.0]));
        assert!(close(d.paths[0].points[1], [20.0, 0.0]));
        assert_eq!(d.paths[0].color, FOREGROUND);
    }

    #[test]
    fn header_units_parsed() {
        let mut g = vec![(0, "SECTION"), (2, "HEADER"), (9, "$ACADVER"), (1, "AC1015"), (9, "$INSUNITS"), (70, "4"), (0, "ENDSEC")];
        g.extend([(0, "SECTION"), (2, "ENTITIES"), (0, "POINT"), (10, "1"), (20, "1"), (0, "ENDSEC"), (0, "EOF")]);
        let d = decode(&dxf(&g)).unwrap();
        assert_eq!(d.units, "mm");
        assert_eq!(d.info[0], ("DXF version".to_string(), "AC1015 (AutoCAD 2000)".to_string()));
    }

    #[test]
    fn solid_is_filled_in_zig_zag_order() {
        let d = decode(&entities(&[(0, "SOLID"), (10, "0"), (20, "0"), (11, "4"), (21, "0"), (12, "0"), (22, "4"), (13, "4"), (23, "4")])).unwrap();
        assert!(d.paths.is_empty());
        assert_eq!(d.fills[0].rings[0], vec![[0.0, 4.0], [4.0, 4.0], [4.0, 0.0], [0.0, 0.0]]);
    }

    #[test]
    fn polyface_edges_are_shared_and_hidden() {
        let verts = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        // Two triangles sharing the diagonal 1–3 (drawn once); the second hides its edge 3–4.
        let edges = polyface_edges(&verts, &[[1, 2, 3, 0], [-3, 4, 1, 0]]);
        assert_eq!(edges, vec![vec![verts[0], verts[1]], vec![verts[1], verts[2]], vec![verts[2], verts[0]], vec![verts[3], verts[0]]]);
        let grid = mesh_grid(&verts, 2, 2, false, true);
        assert_eq!(grid.len(), 4);
        assert_eq!(grid[0].len(), 3, "closed in N repeats the first vertex");
    }

    #[test]
    fn wipeout_rectangle_from_clip_corners() {
        let ring = wipeout_ring([10.0, 20.0], [5.0, 0.0], [0.0, 2.0], [1.0, 1.0], &[[-0.5, -0.5], [0.5, 0.5]]);
        let mut ring = ring.clone();
        ring.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ring, vec![[10.0, 20.0], [10.0, 22.0], [15.0, 20.0], [15.0, 22.0]]);
    }

    #[test]
    fn dimension_without_block_is_drawn_from_its_points() {
        let d = decode(&entities(&[
            (0, "DIMENSION"), (2, "*D9"), (10, "0"), (20, "5"), (11, "5"), (21, "5"), (70, "0"), (1, "<> mm"),
            (13, "0"), (23, "0"), (14, "10"), (24, "0"), (50, "0"),
        ]))
        .unwrap();
        assert_eq!(d.texts[0].text, "10.0000 mm");
        assert_eq!(d.fills.len(), 2, "two arrowheads");
    }

    #[test]
    fn lists_entities_not_drawn() {
        let d = decode(&entities(&[(0, "OLE2FRAME"), (0, "OLE2FRAME"), (0, "IMAGE"), (0, "POINT"), (10, "0"), (20, "0")])).unwrap();
        assert_eq!(d.info.last().unwrap().1, "IMAGE × 1, OLE2FRAME × 2");
    }

    /// A HATCH of the square (0,0)–(4,4) with the hole (1,1)–(3,3): a polyline path, an edge path, then `tail`.
    fn square_hatch(tail: &[(i32, &'static str)]) -> Vec<u8> {
        let mut g = vec![
            (0, "HATCH"), (8, "0"), (10, "0"), (20, "0"), (30, "0"), (2, "TEST"), (70, "0"), (71, "0"), (91, "2"),
            (92, "2"), (72, "0"), (73, "1"), (93, "4"), (10, "0"), (20, "0"), (10, "4"), (20, "0"), (10, "4"), (20, "4"), (10, "0"), (20, "4"), (97, "0"),
            (92, "0"), (93, "4"),
            (72, "1"), (10, "1"), (20, "1"), (11, "3"), (21, "1"),
            (72, "1"), (10, "3"), (20, "1"), (11, "3"), (21, "3"),
            (72, "1"), (10, "3"), (20, "3"), (11, "1"), (21, "3"),
            (72, "1"), (10, "1"), (20, "3"), (11, "1"), (21, "1"),
            (97, "1"), (330, "1F"),
        ];
        g.extend_from_slice(tail);
        entities(&g)
    }

    #[test]
    fn hatch_pattern_clipped_to_boundary_with_hole() {
        // One family: horizontal lines through (0, 0.5) + k·(0, 1).
        let pattern = [(75, "0"), (76, "1"), (52, "0"), (41, "1"), (77, "0"), (78, "1"), (53, "0"), (43, "0"), (44, "0.5"), (45, "0"), (46, "1"), (79, "0"), (98, "0")];
        let d = decode(&square_hatch(&pattern)).unwrap();
        let world = |q: P| [q[0] + d.origin[0], d.origin[1] - q[1]];
        let mut spans: Vec<_> = d.paths.iter().map(|p| (world(p.points[0]), world(p.points[1]))).collect();
        spans.sort_by(|a, b| (a.0[1], a.0[0]).partial_cmp(&(b.0[1], b.0[0])).unwrap());
        let want = [((0.0, 0.5), 4.0), ((0.0, 1.5), 1.0), ((3.0, 1.5), 4.0), ((0.0, 2.5), 1.0), ((3.0, 2.5), 4.0), ((0.0, 3.5), 4.0)];
        assert_eq!(spans.len(), want.len(), "{spans:?}");
        for ((a, b), ((x, y), end)) in spans.iter().zip(want) {
            assert!(close(*a, [x, y]) && close(*b, [end, y]), "{a:?} {b:?}");
        }
    }

    #[test]
    fn solid_hatch_is_even_odd_fill() {
        let d = decode(&String::from_utf8(square_hatch(&[])).unwrap().replacen("70\n0\n", "70\n1\n", 1).into_bytes()).unwrap();
        assert_eq!((d.fills.len(), d.fills[0].rings.len(), d.fills[0].even_odd), (1, 2, true));
    }

    /// A MULTILEADER from (0, 0) to the connection point (10, 5), a dogleg of 4 to the right, the
    /// text "Note" and arrowhead `arrow` (a block record handle, or "0" for the default).
    fn multileader_dxf(arrow: &'static str) -> Vec<u8> {
        dxf(&[
            (0, "SECTION"), (2, "BLOCKS"),
            (0, "BLOCK"), (5, "30"), (330, "2F"), (2, "_Dot"), (10, "0"), (20, "0"),
            (0, "CIRCLE"), (8, "0"), (10, "0"), (20, "0"), (40, "0.5"),
            (0, "ENDBLK"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "MULTILEADER"), (8, "0"), (62, "1"), (300, "CONTEXT_DATA{"), (40, "1"), (10, "14"), (20, "5"), (41, "2"), (140, "3"),
            (290, "1"), (304, "Note"), (12, "15"), (22, "6"), (13, "1"), (23, "0"), (90, "-1056964608"), (171, "1"), (296, "0"),
            (302, "LEADER{"), (290, "1"), (10, "10"), (20, "5"), (11, "1"), (21, "0"), (90, "0"), (40, "4"),
            (304, "LEADER_LINE{"), (10, "0"), (20, "0"), (91, "0"), (341, "0"), (305, "}"),
            (303, "}"), (301, "}"),
            (91, "-1023410171"), (290, "1"), (291, "1"), (342, arrow),
            (0, "ENDSEC"), (0, "EOF"),
        ])
    }

    #[test]
    fn multileader_lines_arrow_and_text() {
        let d = decode(&multileader_dxf("0")).unwrap();
        let world = |q: P| [q[0] + d.origin[0], d.origin[1] - q[1]];
        let lines: Vec<Vec<P>> = d.paths.iter().map(|p| p.points.iter().map(|&q| world(q)).collect()).collect();
        assert_eq!(lines, [vec![[10.0, 5.0], [14.0, 5.0]], vec![[0.0, 0.0], [10.0, 5.0]]]);
        // Leader colour 0xC3000005 is ACI 5 (blue); the text is BYBLOCK, so the entity's red.
        assert_eq!(d.paths[0].color, 0x0000FF);
        assert_eq!((d.texts[0].text.as_str(), d.texts[0].color, d.texts[0].valign), ("Note", 0xFF0000, 3));
        // The default arrow is a filled triangle with its tip on the leader's first point.
        assert!(close(world(d.fills[0].rings[0][0]), [0.0, 0.0]) && d.fills[0].color == 0x0000FF);
        // An arrowhead block replaces it, scaled by the arrow size.
        let d = decode(&multileader_dxf("2F")).unwrap();
        assert!(d.fills.is_empty() && (d.arcs[0].u[0].hypot(d.arcs[0].u[1]) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn ray_and_xline_reach_past_extents_without_growing_them() {
        let d = decode(&entities(&[
            (0, "LINE"), (8, "0"), (10, "0"), (20, "0"), (11, "10"), (21, "10"),
            (0, "RAY"), (8, "0"), (10, "5"), (20, "5"), (11, "1"), (21, "0"),
            (0, "XLINE"), (8, "0"), (10, "5"), (20, "5"), (11, "0"), (21, "1"),
        ]))
        .unwrap();
        assert_eq!((d.width, d.height), (10.0, 10.0));
        let world = |q: P| [q[0] + d.origin[0], d.origin[1] - q[1]];
        let ends = |i: usize| [world(d.paths[i].points[0]), world(d.paths[i].points[1])];
        // RAY_REACH (10) drawing sizes beyond the extents (0, 0)–(10, 10).
        assert_eq!(ends(1), [[5.0, 5.0], [110.0, 5.0]]);
        assert_eq!(ends(2), [[5.0, -100.0], [5.0, 110.0]]);
    }

    #[test]
    fn leader_arrow_sized_by_its_dimension_style() {
        let g = [
            (0, "SECTION"), (2, "HEADER"), (9, "$DIMASZ"), (40, "1"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "TABLES"), (0, "DIMSTYLE"), (2, "ISO"), (41, "2.5"), (40, "2"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "LEADER"), (8, "0"), (3, "ISO"), (71, "1"), (72, "0"), (76, "3"), (10, "0"), (20, "0"), (10, "10"), (20, "0"), (10, "10"), (20, "5"),
            (0, "LEADER"), (8, "0"), (3, "Other"), (10, "0"), (20, "10"), (10, "10"), (20, "10"),
            (0, "LEADER"), (8, "0"), (3, "ISO"), (71, "0"), (10, "0"), (20, "20"), (10, "10"), (20, "20"),
            (0, "ENDSEC"), (0, "EOF"),
        ];
        let d = decode(&dxf(&g)).unwrap();
        let world = |q: P| [q[0] + d.origin[0], d.origin[1] - q[1]];
        assert_eq!(d.paths[0].points.iter().map(|&q| world(q)).collect::<Vec<_>>(), [[0.0, 0.0], [10.0, 0.0], [10.0, 5.0]]);
        // ISO: 2.5 × DIMSCALE 2 = 5, pointing at (0, 0) from (10, 0). An unknown style takes $DIMASZ 1; 71 = 0 has none.
        assert_eq!(d.fills.len(), 2);
        let ring: Vec<P> = d.fills[0].rings[0].iter().map(|&q| world(q)).collect();
        assert!(close(ring[0], [0.0, 0.0]) && close(ring[1], [5.0, -5.0 / 6.0]), "{ring:?}");
        assert!(close(world(d.fills[1].rings[0][1]), [1.0, 10.0 - 1.0 / 6.0]));
    }

    #[test]
    fn hatch_dashes_start_at_line_base() {
        assert_eq!(dash_spans(0.5, 4.0, &[1.0, -1.0]), [(0.5, 1.0), (2.0, 3.0)]);
        // A dash ending on the boundary leaves no dot; a dot pattern keeps its dots.
        assert_eq!(dash_spans(3.0, 4.0, &[1.0, -1.0]), []);
        assert_eq!(dash_spans(0.0, 2.5, &[0.0, -1.0]), [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)]);
        assert_eq!(dash_spans(0.0, 2.5, &[]), [(0.0, 2.5)]);
    }

    #[test]
    fn bulge_is_semicircle() {
        // (0,0) → (2,0) with bulge 1: a counter-clockwise half circle below the chord, centre (1,0).
        let d = decode(&entities(&[(0, "LWPOLYLINE"), (90, "2"), (70, "0"), (10, "0"), (20, "0"), (42, "1"), (10, "2"), (20, "0")])).unwrap();
        assert!(d.paths.is_empty());
        let a = &d.arcs[0];
        assert!((a.t1 - a.t0 - std::f64::consts::PI).abs() < 1e-9);
        // World (1, 0) → output (1, 0 - (-1)) since extents are y ∈ [-1, 0].
        assert!(close(a.centre, [1.0, 0.0]));
        assert!(close(a.at(a.t0), [0.0, 0.0]));
        assert!(close(a.at(a.t1), [2.0, 0.0]));
        assert!(close(a.at((a.t0 + a.t1) / 2.0), [1.0, 1.0]));
        assert!((d.height - 1.0).abs() < 1e-3);
    }

    #[test]
    fn wide_polyline_is_mitred_where_width_carries_on() {
        // (0,0) → (10,0) → (10,10) → (0,10): width 2, then 2 tapering to 4, then 3.
        let d = decode(&entities(&[
            (0, "LWPOLYLINE"), (90, "4"), (70, "0"),
            (10, "0"), (20, "0"), (40, "2"), (41, "2"), (10, "10"), (20, "0"), (40, "2"), (41, "4"),
            (10, "10"), (20, "10"), (40, "3"), (41, "3"), (10, "0"), (20, "10"),
        ]))
        .unwrap();
        assert!(d.paths.is_empty() && d.arcs.is_empty());
        let world = |q: &P| [q[0] + d.origin[0], d.origin[1] - q[1]];
        let ring = |i: usize| d.fills[i].rings[0].iter().map(world).collect::<Vec<_>>();
        // Same width at the first corner: both bands end on its bisector, (9, 1) inside, (11, -1) outside.
        let (first, second) = (ring(0), ring(1));
        assert!(close(first[1], [9.0, 1.0]) && close(first[2], [11.0, -1.0]), "{first:?}");
        assert!(close(second[0], [9.0, 1.0]) && close(*second.last().unwrap(), [11.0, -1.0]), "{second:?}");
        // Width 4 meets 3 at the second corner: both ends square.
        assert!(close(second[second.len() / 2 - 1], [8.0, 10.0]), "{second:?}");
        assert!(close(ring(2)[0], [10.0, 8.5]), "{:?}", ring(2));
    }

    #[test]
    fn wide_polyline_mitres_into_arcs_of_the_same_width() {
        // Width 2 along (0,0) → (10,0), then an arc (bulge −0.5) that leaves the corner turned
        // clockwise: mitred if the arc is 2 wide too, square at x = 10 if it is 3.
        for (arc_width, square) in [("2", false), ("3", true)] {
            let d = decode(&entities(&[
                (0, "LWPOLYLINE"), (90, "3"), (70, "0"),
                (10, "0"), (20, "0"), (40, "2"), (41, "2"), (10, "10"), (20, "0"), (42, "-0.5"), (40, arc_width), (41, arc_width), (10, "20"), (20, "0"),
            ]))
            .unwrap();
            let world = |q: &P| [q[0] + d.origin[0], d.origin[1] - q[1]];
            let first = d.fills[0].rings[0].iter().map(world).collect::<Vec<_>>();
            assert_eq!(close(first[1], [10.0, 1.0]) && close(first[2], [10.0, -1.0]), square, "{first:?}");
        }
    }

    #[test]
    fn linetype_patterns_to_canvas() {
        assert_eq!(canvas_dash([12.0, -3.0]), [12.0, 3.0]);
        assert_eq!(canvas_dash([24.0, -3.0, 0.0, -3.0]), [24.0, 3.0, 0.0, 3.0]);
        assert_eq!(canvas_dash([-3.0, 12.0, 0.0, -1.0]), [12.0, 4.0]);
        assert!(canvas_dash([5.0, 0.0]).is_empty() && canvas_dash([]).is_empty());
    }

    #[test]
    fn line_weight_by_layer_by_block_and_default() {
        let g = [
            (0, "SECTION"), (2, "HEADER"), (9, "$LWDEFAULT"), (70, "30"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "TABLES"),
            (0, "LAYER"), (2, "Heavy"), (62, "7"), (370, "100"),
            (0, "ENDSEC"),
            (0, "SECTION"), (2, "BLOCKS"),
            (0, "BLOCK"), (2, "b"), (10, "0"), (20, "0"),
            (0, "LINE"), (8, "0"), (370, "-2"), (10, "0"), (20, "0"), (11, "1"), (21, "0"),
            (0, "ENDBLK"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "LINE"), (8, "Heavy"), (10, "0"), (20, "0"), (11, "10"), (21, "0"),
            (0, "LINE"), (8, "Heavy"), (370, "50"), (10, "0"), (20, "1"), (11, "10"), (21, "1"),
            (0, "LINE"), (8, "0"), (10, "0"), (20, "2"), (11, "10"), (21, "2"),
            (0, "INSERT"), (8, "0"), (370, "211"), (2, "b"), (10, "0"), (20, "3"),
            (0, "ENDSEC"), (0, "EOF"),
        ];
        let d = decode(&dxf(&g)).unwrap();
        // BYLAYER, its own, BYLAYER on a layer without one ($LWDEFAULT), BYBLOCK from the insert.
        assert_eq!(d.paths.iter().map(|p| p.weight).collect::<Vec<_>>(), [100, 50, 30, 211]);
    }

    #[test]
    fn linetype_by_layer_by_block_and_scale() {
        let g = [
            (0, "SECTION"), (2, "HEADER"), (9, "$LTSCALE"), (40, "2"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "TABLES"),
            (0, "LTYPE"), (2, "DASHED"), (73, "2"), (40, "4"), (49, "3"), (74, "0"), (49, "-1"), (74, "0"),
            (0, "LTYPE"), (2, "FENCE"), (73, "2"), (40, "4"), (49, "3"), (74, "0"), (49, "-1"), (74, "2"),
            (0, "LAYER"), (2, "Hidden"), (62, "7"), (6, "DASHED"),
            (0, "ENDSEC"),
            (0, "SECTION"), (2, "BLOCKS"),
            (0, "BLOCK"), (2, "b"), (10, "0"), (20, "0"),
            (0, "LINE"), (8, "0"), (6, "BYBLOCK"), (10, "0"), (20, "0"), (11, "1"), (21, "0"),
            (0, "ENDBLK"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "LINE"), (8, "Hidden"), (10, "0"), (20, "0"), (11, "10"), (21, "0"),
            (0, "LINE"), (8, "Hidden"), (6, "CONTINUOUS"), (10, "0"), (20, "1"), (11, "10"), (21, "1"),
            (0, "LINE"), (8, "0"), (6, "FENCE"), (10, "0"), (20, "2"), (11, "10"), (21, "2"),
            (0, "INSERT"), (8, "0"), (6, "DASHED"), (48, "0.5"), (2, "b"), (10, "0"), (20, "3"), (41, "3"), (42, "3"),
            (0, "ENDSEC"), (0, "EOF"),
        ];
        let d = decode(&dxf(&g)).unwrap();
        let dash = |i: usize| d.dashes[d.paths[i].dash as usize].clone();
        // BYLAYER × $LTSCALE 2; CONTINUOUS and the complex FENCE are solid.
        assert_eq!(dash(0), [6.0, 2.0]);
        assert!(dash(1).is_empty() && dash(2).is_empty());
        // BYBLOCK takes the INSERT's DASHED, scaled by $LTSCALE and the insert scale 3, not its own 0.5.
        assert_eq!(dash(3), [18.0, 6.0]);
    }

    #[test]
    fn complex_linetype_places_shapes_at_element_ends() {
        let zig = LinetypeShape { strokes: shapes::strokes("ltypeshp.shx", 131).unwrap(), scale: 0.5, rotation: 0.0, absolute: false, offset: [0.0, 0.0] };
        let elements = [LinetypeElement::plain(0.0), LinetypeElement { length: -2.0, shape: Some(zig) }];
        // Up the Y axis: dots at 0 and 2, a zig ending at 2 (whose end is the path's end), none past it.
        let strokes = complex_strokes(&[[0.0, 0.0], [0.0, 3.0]], &elements);
        assert_eq!(strokes.len(), 3);
        assert_eq!(strokes[0], [[0.0, 0.0]; 2]);
        let p = &strokes[1];
        assert!(close(p[0], [0.0, 2.0]) && close(p[1], [-0.5, 2.5]) && close(p[2], [0.0, 3.0]), "{p:?}");
        assert_eq!(strokes[2], [[0.0, 2.0]; 2]);
    }

    #[test]
    fn complex_linetype_from_ltype_table() {
        let line = |lt: &'static str| [(0, "LINE"), (8, "0"), (6, lt), (10, "0"), (20, "0"), (11, "10"), (21, "0")];
        let g: Vec<(i32, &str)> = [
            &[(0, "SECTION"), (2, "TABLES"), (0, "STYLE"), (5, "1A"), (2, ""), (70, "1"), (3, "ltypeshp.shx"),
            (0, "LTYPE"), (2, "ZIG"), (73, "2"), (40, "4"), (49, "0"), (74, "0"), (49, "-4"), (74, "4"), (75, "131"), (340, "1A"), (46, "1"), (50, "0"), (44, "0"), (45, "0"),
            (0, "LTYPE"), (2, "OTHER"), (73, "2"), (40, "4"), (49, "0"), (74, "0"), (49, "-4"), (74, "4"), (75, "1"), (340, "1A"), (46, "1"), (50, "0"), (44, "0"), (45, "0"),
            (0, "ENDSEC"), (0, "SECTION"), (2, "ENTITIES")][..],
            &line("ZIG"), &line("OTHER"), &[(0, "ENDSEC"), (0, "EOF")],
        ]
        .concat();
        let d = decode(&dxf(&g)).unwrap();
        // ZIG: dots at 0, 4 and 8 and zigs ending at 4 and 8. OTHER's shape 1 is not built in: solid.
        assert_eq!(d.paths.len(), 6);
        assert_eq!(d.paths.iter().filter(|p| p.points.len() == 3).count(), 2);
        assert!(d.paths.iter().all(|p| p.dash == 0));
    }

    #[test]
    fn insert_transforms_block() {
        let g = [
            (0, "SECTION"), (2, "BLOCKS"),
            (0, "BLOCK"), (2, "b"), (10, "1"), (20, "0"),
            (0, "LINE"), (8, "0"), (62, "0"), (10, "1"), (20, "0"), (11, "2"), (21, "0"),
            (0, "ENDBLK"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            // Base point (1,0) lands on (10,0); the unit line is rotated 90° and doubled.
            (0, "INSERT"), (8, "0"), (62, "1"), (2, "B"), (10, "10"), (20, "0"), (41, "2"), (42, "2"), (50, "90"),
            (0, "ENDSEC"), (0, "EOF"),
        ];
        let d = decode(&dxf(&g)).unwrap();
        let p = &d.paths[0].points;
        let world = |q: P| [q[0] + d.origin[0], d.origin[1] - q[1]];
        assert!(close(world(p[0]), [10.0, 0.0]));
        assert!(close(world(p[1]), [10.0, 2.0]));
        // BYBLOCK takes the INSERT's red.
        assert_eq!(d.paths[0].color, 0xFF0000);
    }

    #[test]
    fn layer_colour_and_visibility() {
        let g = [
            (0, "SECTION"), (2, "TABLES"), (0, "TABLE"), (2, "LAYER"),
            (0, "LAYER"), (2, "Walls"), (70, "0"), (62, "5"),
            (0, "LAYER"), (2, "Hidden"), (70, "0"), (62, "-1"),
            (0, "ENDTAB"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "LINE"), (8, "WALLS"), (10, "0"), (20, "0"), (11, "1"), (21, "0"),
            (0, "LINE"), (8, "Hidden"), (10, "0"), (20, "0"), (11, "1"), (21, "1"),
            (0, "ENDSEC"), (0, "EOF"),
        ];
        let d = decode(&dxf(&g)).unwrap();
        let names: Vec<_> = d.layers.iter().map(|l| (l.name.as_str(), l.visible)).collect();
        assert_eq!(names, [("Hidden", false), ("Walls", true)]);
        assert_eq!(d.paths.len(), 2);
        assert_eq!((d.paths[0].color, d.paths[0].layer), (0x0000FF, 1));
        assert_eq!(d.paths[1].layer, 0);
        // Extents only cover the shown line.
        assert_eq!(d.height, 1e-3);
    }

    #[test]
    fn block_contents_keep_their_own_layer() {
        let g = [
            (0, "SECTION"), (2, "TABLES"), (0, "TABLE"), (2, "LAYER"),
            (0, "LAYER"), (2, "Frozen"), (70, "1"), (62, "1"),
            (0, "ENDTAB"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "BLOCKS"),
            (0, "BLOCK"), (2, "b"), (10, "0"), (20, "0"),
            (0, "LINE"), (8, "Inner"), (10, "0"), (20, "0"), (11, "1"), (21, "0"),
            (0, "LINE"), (8, "0"), (10, "0"), (20, "0"), (11, "0"), (21, "1"),
            (0, "ENDBLK"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ENTITIES"),
            (0, "INSERT"), (8, "Frozen"), (2, "B"), (10, "0"), (20, "0"),
            (0, "POINT"), (8, "0"), (10, "5"), (20, "5"),
            (0, "ENDSEC"), (0, "EOF"),
        ];
        let d = decode(&dxf(&g)).unwrap();
        let names: Vec<_> = d.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Frozen", "Inner", "0"]);
        let layers: Vec<_> = d.paths.iter().map(|p| p.layer).collect();
        // The inner line stays on its own layer; the layer-0 line takes the INSERT's layer.
        assert_eq!(layers, [1, 0, 2]);
        // The inner line and the point are shown; the layer-0 line on the frozen layer is not.
        assert!(close([d.width, d.height], [5.0, 5.0]) && close(d.origin, [0.0, 5.0]));
    }

    #[test]
    fn mirrored_arc() {
        // Extrusion (0,0,-1) mirrors OCS x: centre (5,0) is at world (-5,0).
        let d = decode(&entities(&[(0, "CIRCLE"), (10, "5"), (20, "0"), (40, "1"), (230, "-1")])).unwrap();
        assert!(close([d.origin[0], d.origin[1]], [-6.0, 1.0]));
    }

    #[test]
    fn spline_hits_end_points() {
        let pts = decode(&entities(&[
            (0, "SPLINE"), (71, "2"), (72, "6"), (73, "3"),
            (40, "0"), (40, "0"), (40, "0"), (40, "1"), (40, "1"), (40, "1"),
            (10, "0"), (20, "0"), (10, "1"), (20, "2"), (10, "2"), (20, "0"),
        ]))
        .unwrap()
        .paths
        .remove(0)
        .points;
        assert_eq!(pts.len(), SPLINE_STEPS + 1);
        assert!(close(pts[0], [0.0, 1.0]));
        assert!(close(pts[SPLINE_STEPS], [2.0, 1.0]));
        // Quadratic Bézier midpoint: (1, 1) in world, 1 below the top at y = 1.
        assert!(close(pts[SPLINE_STEPS / 2], [1.0, 0.0]));
    }

    #[test]
    fn spline_through_fit_points() {
        let fit = [[0.0, 0.0], [1.0, 1.0], [2.0, 0.0], [3.0, 1.0]];
        let pts = through(&fit);
        assert_eq!(pts.len(), 3 * SPLINE_STEPS + 1);
        for (i, p) in fit.iter().enumerate() {
            assert!(close(pts[i * SPLINE_STEPS], *p));
        }
    }

    #[test]
    fn text_alignment_and_codes() {
        let d = decode(&entities(&[(0, "TEXT"), (10, "0"), (20, "0"), (11, "5"), (21, "5"), (40, "2"), (1, "%%c10"), (72, "1"), (73, "2")])).unwrap();
        let t = &d.texts[0];
        assert_eq!((t.text.as_str(), t.halign, t.valign), ("Ø10", 1, 2));
        assert!(close(t.up, [0.0, -2.0]));
    }

    #[test]
    fn oblique_text_slants_its_up_vector() {
        // Text rotated 90° (baseline along +Y, up along −X), slanted 45° towards its baseline:
        // up is (−2, 2) in world, (−2, −2) in output coordinates (Y down).
        let d = decode(&entities(&[(0, "TEXT"), (10, "0"), (20, "0"), (40, "2"), (1, "x"), (50, "90"), (51, "45")])).unwrap();
        let t = &d.texts[0];
        assert!(close(t.x, [0.0, -2.0]) && close(t.up, [-2.0, -2.0]), "{:?} {:?}", t.x, t.up);
    }

    /// A DXF with one `kind` entity whose ACIS data is `body`, as SAB in the ACDSDATA section.
    fn acis_dxf(kind: &str, body: &acadrust::entities::acis::SatDocument) -> Drawing {
        let hex: String = acadrust::entities::acis::SabWriter::write(body).iter().map(|b| format!("{b:02X}")).collect();
        decode(&dxf(&[
            (0, "SECTION"), (2, "ENTITIES"), (0, kind), (5, "2A"), (8, "0"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "ACDSDATA"), (0, "ACDSRECORD"), (320, "2A"), (2, "ASM_Data"), (310, &hex), (0, "ENDSEC"), (0, "EOF"),
        ]))
        .unwrap()
    }

    #[test]
    fn solid_edges_placed_by_body_transform() {
        use acadrust::entities::acis::{SatDocument, SatPointer, Sense};
        // One edge from (0, 0, 5) to (1, 0, 5), rotated a quarter turn about Z and moved by (10, 20).
        let mut body = SatDocument::new_body();
        let ptr = |i: i32| SatPointer::new(i);
        let (a, b) = (body.add_point(0.0, 0.0, 5.0), body.add_point(1.0, 0.0, 5.0));
        let (va, vb) = (body.add_vertex(SatPointer::NULL, ptr(a)), body.add_vertex(SatPointer::NULL, ptr(b)));
        let line = body.add_straight_curve([0.0, 0.0, 5.0], [1.0, 0.0, 0.0]);
        body.add_edge(ptr(va), 0.0, ptr(vb), 1.0, SatPointer::NULL, ptr(line), Sense::Forward);
        body.set_placement([[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]], [10.0, 20.0, 0.0], 1.0);
        let d = acis_dxf("3DSOLID", &body);
        // World (10, 20)–(10, 21): vertical, drawn y down from the top of the extents.
        let p = &d.paths[0].points;
        assert!(close(p[0], [0.0, 1.0]) && close(p[1], [0.0, 0.0]), "{p:?}");
    }

    #[test]
    fn region_edges_from_acds_data() {
        use acadrust::entities::acis::{SatDocument, SatPointer, Sense};
        // A half disc: the diameter from (2, 0) to (-2, 0), and the arc back through (0, 2).
        let mut body = SatDocument::new_body();
        let ptr = |i: i32| SatPointer::new(i);
        let (a, b) = (body.add_point(2.0, 0.0, 0.0), body.add_point(-2.0, 0.0, 0.0));
        let (va, vb) = (body.add_vertex(SatPointer::NULL, ptr(a)), body.add_vertex(SatPointer::NULL, ptr(b)));
        let line = body.add_straight_curve([2.0, 0.0, 0.0], [-1.0, 0.0, 0.0]);
        let circle = body.add_ellipse_curve([0.0; 3], [0.0, 0.0, 1.0], [2.0, 0.0, 0.0], 1.0);
        body.add_edge(ptr(va), 0.0, ptr(vb), 4.0, SatPointer::NULL, ptr(line), Sense::Forward);
        body.add_edge(ptr(vb), 0.0, ptr(va), 0.0, SatPointer::NULL, ptr(circle), Sense::Reversed);
        let d = acis_dxf("REGION", &body);
        assert_eq!(d.paths.len(), 2);
        assert_eq!(d.paths[0].points.len(), 2);
        // Reversed against the anticlockwise circle, the arc from (-2, 0) to (2, 0) runs clockwise,
        // over the top: 2 above the diameter, which is drawn y down.
        let diameter = d.paths[0].points[0][1];
        let ys: Vec<f64> = d.paths[1].points.iter().map(|p| p[1]).collect();
        assert!((ys.iter().copied().fold(f64::INFINITY, f64::min) - (diameter - 2.0)).abs() < 1e-9);
        assert!(ys.iter().all(|&y| y <= diameter + 1e-9));
    }

    #[test]
    fn mtext_wraps_at_reference_width() {
        let d = decode(&entities(&[(0, "MTEXT"), (10, "0"), (20, "0"), (40, "2"), (41, "30"), (1, "Some words")])).unwrap();
        assert_eq!((d.texts[0].wrap, d.texts[0].fit), (30.0, 0.0));
    }

    #[test]
    fn mtext_formatting() {
        assert_eq!(parse::mtext_plain("{\\fArial|b1;Bold}\\PLine \\S1/2; \\H2.5x;big\\~x \\\\"), "Bold\nLine 1/2 big\u{a0}x \\");
        assert_eq!(parse::mtext_plain("\\U+00C5ngstr\\U+00F6m"), "Ångström");
    }

    #[test]
    fn rejects_binary_and_non_dxf() {
        assert!(decode(b"AutoCAD Binary DXF\r\n\x1a\0").err().unwrap().contains("Binary"));
        assert!(decode(b"hello").is_err());
        assert!(decode(&entities(&[])).err().unwrap().contains("no visible"));
    }
}
