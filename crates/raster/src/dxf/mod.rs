//! DXF (ASCII) model-space geometry for vector display.
//!
//! Blocks are expanded into world coordinates. Lines and polylines become paths; circles, arcs,
//! ellipses and polyline bulges stay exact as affine images of the unit circle, so they are sharp
//! at any zoom and keep their centres and radii (for later measuring and snapping). Splines are
//! flattened. Output coordinates are relative to the top-left of the drawing extents with Y down,
//! like image pixels, which keeps them small enough for the canvas' single-precision paths.
//!
//! The output side (`World`, `Affine`, `style`, text and spline helpers) is shared with `dwg`.

mod color;
pub(crate) mod parse;

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;

pub(crate) use color::aci;
pub use color::FOREGROUND;
use parse::{Pair, Record};

pub(crate) type P = [f64; 2];

/// Maximum INSERT nesting; deeper (or self-referencing) blocks are skipped.
pub(crate) const MAX_DEPTH: u32 = 16;
/// Segments per knot span when flattening splines.
const SPLINE_STEPS: usize = 16;

pub struct Path {
    pub points: Vec<P>,
    pub color: u32,
    /// Index into `Drawing::layers`.
    pub layer: u32,
}

/// The points `centre + u·cos t + v·sin t` for `t` from `t0` to `t1` (which may be less than `t0`).
pub struct Arc {
    pub centre: P,
    pub u: P,
    pub v: P,
    pub t0: f64,
    pub t1: f64,
    pub color: u32,
    /// Index into `Drawing::layers`.
    pub layer: u32,
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
    /// Layers in name order, with their visibility as saved in the file.
    pub layers: Vec<Layer>,
    /// File properties as (label, value) rows for display.
    pub info: Vec<(String, String)>,
}

/// Colour and layer of a shape.
#[derive(Clone, Copy)]
pub(crate) struct Pen {
    pub color: u32,
    pub layer: u32,
}

/// Parses an ASCII DXF file and expands its model space.
pub fn decode(data: &[u8]) -> Result<Drawing, String> {
    let text = parse::decode_text(data)?;
    let pairs = parse::pairs(&text)?;
    let records = parse::records(&pairs);

    let mut header: &[Pair] = &[];
    let mut layers = Vec::new();
    let mut blocks = HashMap::new();
    let mut entities = Vec::new();
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
            Some("TABLES") => layers.extend(body.iter().filter(|r| r.kind == "LAYER").map(layer)),
            Some("BLOCKS") => blocks = parse_blocks(body),
            Some("ENTITIES") => entities = group(body),
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

    let mut b = Builder { blocks: &blocks, world: World::new(layers) };
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
    /// Entity types that are not drawn, with how often they occur (block contents once per insert).
    skipped: BTreeMap<&'a str, usize>,
    layers: Vec<Layer>,
    /// Upper-case layer name → index in `layers`.
    layer_ids: HashMap<String, u32>,
}

impl<'a> World<'a> {
    /// `layers` is the file's layer table; layers used but not listed are added as they are met.
    pub(crate) fn new(mut layers: Vec<Layer>) -> Self {
        layers.sort_by_cached_key(|l| l.name.to_uppercase());
        layers.dedup_by(|a, b| a.name.to_uppercase() == b.name.to_uppercase());
        let layer_ids = layers.iter().enumerate().map(|(i, l)| (l.name.to_uppercase(), i as u32)).collect();
        let (paths, arcs, texts, skipped) = Default::default();
        World { paths, arcs, texts, skipped, layers, layer_ids }
    }

    fn layer_id(&mut self, name: &str) -> u32 {
        let key = name.to_uppercase();
        if let Some(&id) = self.layer_ids.get(&key) {
            return id;
        }
        let id = self.layers.len() as u32;
        self.layers.push(Layer { name: name.to_string(), color: FOREGROUND, visible: true });
        self.layer_ids.insert(key, id);
        id
    }

    pub(crate) fn skip(&mut self, kind: &'a str) {
        *self.skipped.entry(kind).or_default() += 1;
    }

    pub(crate) fn path(&mut self, points: Vec<P>, m: &Affine, pen: Pen) {
        if points.len() >= 2 {
            self.paths.push(Path { points: points.into_iter().map(|p| m.apply(p)).collect(), color: pen.color, layer: pen.layer });
        }
    }

    fn arc(&mut self, a: Arc, m: &Affine) {
        self.arcs.push(Arc { centre: m.apply(a.centre), u: m.linear(a.u), v: m.linear(a.v), ..a });
    }

    pub(crate) fn circle_arc(&mut self, centre: P, r: f64, start: f64, sweep: f64, m: &Affine, pen: Pen) {
        if r > 0.0 {
            self.arc(Arc { centre, u: [r, 0.0], v: [0.0, r], t0: start, t1: start + sweep, color: pen.color, layer: pen.layer }, m);
        }
    }

    /// `ratio` is minor / major axis, negated for a flipped extrusion: the minor axis is the
    /// extrusion direction × the major axis.
    pub(crate) fn ellipse(&mut self, centre: P, u: P, ratio: f64, t0: f64, t1: f64, m: &Affine, pen: Pen) {
        self.arc(Arc { centre, u, v: [-u[1] * ratio, u[0] * ratio], t0, t1: t0 + sweep(t0, t1), color: pen.color, layer: pen.layer }, m);
    }

    /// Straight runs become paths; each bulged segment (bulge = tan(sweep / 4)) becomes an arc.
    pub(crate) fn polyline(&mut self, verts: &[(P, f64)], closed: bool, m: &Affine, pen: Pen) {
        if verts.is_empty() {
            return;
        }
        let segments = if closed { verts.len() } else { verts.len() - 1 };
        let mut run = vec![verts[0].0];
        for i in 0..segments {
            let ([x1, y1], bulge) = verts[i];
            let [x2, y2] = verts[(i + 1) % verts.len()].0;
            if bulge.abs() < 1e-9 {
                run.push([x2, y2]);
                continue;
            }
            self.path(std::mem::replace(&mut run, vec![[x2, y2]]), m, pen);
            let k = (1.0 - bulge * bulge) / (4.0 * bulge);
            let centre = [(x1 + x2) / 2.0 - (y2 - y1) * k, (y1 + y2) / 2.0 + (x2 - x1) * k];
            let r = (x2 - x1).hypot(y2 - y1) * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
            let start = (y1 - centre[1]).atan2(x1 - centre[0]);
            self.circle_arc(centre, r, start, 4.0 * bulge.atan(), m, pen);
        }
        self.path(run, m, pen);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_text(&mut self, pos: P, rotation: f64, height: f64, width: f64, (halign, valign): (u8, u8), text: String, m: &Affine, pen: Pen) {
        if text.trim().is_empty() || height <= 0.0 {
            return;
        }
        let (s, c) = rotation.sin_cos();
        let x = m.linear([c * height * width, s * height * width]);
        let up = m.linear([-s * height, c * height]);
        self.texts.push(Text { pos: m.apply(pos), x, up, halign, valign, text, color: pen.color, layer: pen.layer });
    }

    pub(crate) fn finish(self, units: &'static str, mut info: Vec<(String, String)>) -> Result<Drawing, String> {
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
        (min[0] <= max[0] && min[1] <= max[1]).then_some((min, max))
    }
}

impl Arc {
    fn at(&self, t: f64) -> P {
        let (s, c) = t.sin_cos();
        [self.centre[0] + self.u[0] * c + self.v[0] * s, self.centre[1] + self.u[1] * c + self.v[1] * s]
    }
}

/// Sweep from `t0` to `t1` counter-clockwise; equal angles mean a full turn.
pub(crate) fn sweep(t0: f64, t1: f64) -> f64 {
    let sweep = (t1 - t0).rem_euclid(TAU);
    if sweep == 0.0 { TAU } else { sweep }
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
}

fn layer(r: &Record) -> Layer {
    let index = r.i(62, 7);
    let frozen = r.i(70, 0) & 1 != 0;
    // A negative colour index means the layer is off.
    let name = r.get(2).unwrap_or("").trim().to_string();
    Layer { name, color: true_color(r).unwrap_or_else(|| color::aci(index.abs())), visible: index >= 0 && !frozen }
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
    entities: Vec<Entity<'a>>,
}

fn parse_blocks<'a>(records: &[Record<'a>]) -> HashMap<String, Block<'a>> {
    let mut blocks = HashMap::new();
    let mut rest = records;
    while let Some(start) = rest.iter().position(|r| r.kind == "BLOCK") {
        let head = rest[start];
        let body = &rest[start + 1..];
        let end = body.iter().position(|r| r.kind == "ENDBLK").unwrap_or(body.len());
        blocks.insert(head.name(2), Block { base: head.point(10), entities: group(&body[..end]) });
        rest = &body[end..];
    }
    blocks
}

/// 2D affine transform in canvas order: x' = a·x + c·y + e, y' = b·x + d·y + f.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Affine([f64; 6]);

impl Affine {
    pub(crate) const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn translate([x, y]: P) -> Affine {
        Affine([1.0, 0.0, 0.0, 1.0, x, y])
    }

    fn rotate(angle: f64) -> Affine {
        let (s, c) = angle.sin_cos();
        Affine([c, s, -s, c, 0.0, 0.0])
    }

    fn scale(sx: f64, sy: f64) -> Affine {
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

    fn apply(&self, [x, y]: P) -> P {
        let [a, b, c, d, e, f] = self.0;
        [a * x + c * y + e, b * x + d * y + f]
    }

    fn linear(&self, [x, y]: P) -> P {
        let [a, b, c, d, ..] = self.0;
        [a * x + c * y, b * x + d * y]
    }
}

/// Inherited state while expanding blocks.
pub(crate) struct Ctx<'a> {
    pub m: Affine,
    /// Layer of the enclosing INSERT, which entities on layer 0 take on.
    pub layer: Option<&'a str>,
    /// Colour of the enclosing INSERT, for BYBLOCK.
    pub color: u32,
    pub depth: u32,
}

impl Ctx<'_> {
    pub(crate) const ROOT: Ctx<'static> = Ctx { m: Affine::IDENTITY, layer: None, color: FOREGROUND, depth: 0 };
}

/// Effective layer name, colour and layer index. `index` is the ACI colour (0 BYBLOCK, 256 or
/// negative BYLAYER); `true_color` overrides it.
pub(crate) fn style<'a>(w: &mut World, own: &'a str, true_color: Option<u32>, index: i64, ctx: &Ctx<'a>) -> (&'a str, Pen) {
    let name = match ctx.layer {
        Some(parent) if own == "0" => parent,
        _ => own,
    };
    let layer = w.layer_id(name);
    let by_layer = w.layers[layer as usize].color;
    let color = true_color.unwrap_or_else(|| match index {
        0 => ctx.color,
        256.. | ..0 => by_layer,
        index => color::aci(index),
    });
    (name, Pen { color, layer })
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

struct Builder<'a, 'b> {
    blocks: &'b HashMap<String, Block<'a>>,
    world: World<'a>,
}

impl<'a, 'b> Builder<'a, 'b> {
    fn entity(&mut self, e: &Entity<'a>, ctx: &Ctx<'a>) {
        let r = &e.rec;
        // Paper space and invisible entities are not drawn.
        if r.i(67, 0) == 1 || r.i(60, 0) == 1 {
            return;
        }
        let own = r.get(8).map_or("0", str::trim);
        let (layer, pen) = style(&mut self.world, own, true_color(r), r.i(62, 256), ctx);
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
                let mut verts: Vec<(P, f64)> = Vec::new();
                for p in r.pairs {
                    let v = || p.value.trim().parse().unwrap_or(0.0);
                    match (p.code, verts.last_mut()) {
                        (10, _) => verts.push(([v(), 0.0], 0.0)),
                        (20, Some(last)) => last.0[1] = v(),
                        (42, Some(last)) => last.1 = v(),
                        _ => {}
                    }
                }
                w.polyline(&verts, r.i(70, 0) & 1 != 0, &m.then(&Affine::ocs(r)), pen);
            }
            "POLYLINE" => {
                let flags = r.i(70, 0);
                // Polygon and polyface meshes (16, 64) are 3D; not drawn.
                if flags & (16 | 64) != 0 {
                    return;
                }
                // Spline frame control points (vertex flag 16) are not part of the curve.
                let verts: Vec<_> = e.children.iter().filter(|v| v.kind == "VERTEX" && v.i(70, 0) & 16 == 0).map(|v| (v.point(10), v.f(42, 0.0))).collect();
                let ocs = if flags & 8 != 0 { Affine::IDENTITY } else { Affine::ocs(r) };
                w.polyline(&verts, flags & 1 != 0, &m.then(&ocs), pen);
            }
            // SOLID and TRACE list their corners in zig-zag order; a triangle repeats the third.
            "SOLID" | "TRACE" => {
                let c = r.point(12);
                let d = if r.get(13).is_some() { r.point(13) } else { c };
                w.path(vec![r.point(10), r.point(11), d, c, r.point(10)], &m.then(&Affine::ocs(r)), pen);
            }
            "3DFACE" => {
                let pts = [10, 11, 12, 13, 10].map(|code| r.point(code));
                w.path(pts.to_vec(), &m, pen);
            }
            "SPLINE" => w.path(spline(r), &m, pen),
            "TEXT" | "ATTRIB" => {
                // ATTRIB keeps its vertical alignment in 74; TEXT in 73.
                let valign = r.i(if r.kind == "ATTRIB" { 74 } else { 73 }, 0);
                let (pos, rotation, align) = text_anchor(r.i(72, 0), valign, r.point(10), r.point(11), r.f(50, 0.0).to_radians());
                let string = parse::text_codes(r.get(1).unwrap_or(""));
                w.push_text(pos, rotation, r.f(40, 1.0), r.f(41, 1.0), align, string, &m.then(&Affine::ocs(r)), pen);
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
                w.push_text(r.point(10), rotation, r.f(40, 1.0), 1.0, align, parse::mtext_plain(&raw), &m, pen);
            }
            "INSERT" => {
                let Some(block) = self.blocks.get(&r.name(2)) else { return };
                let grid = (r.i(70, 1), r.i(71, 1));
                let scale = [r.f(41, 1.0), r.f(42, 1.0)];
                let step = [r.f(44, 0.0), r.f(45, 0.0)];
                for cell in insert_cells(&m, Affine::ocs(r), r.point(10), r.f(50, 0.0).to_radians(), scale, block.base, grid, step) {
                    self.expand(&r.name(2), cell, layer, pen, ctx);
                }
                for attrib in &e.children {
                    // Attribute flag 1: invisible.
                    if attrib.kind == "ATTRIB" && attrib.i(70, 0) & 1 == 0 {
                        self.entity(&Entity { rec: *attrib, children: Vec::new() }, ctx);
                    }
                }
            }
            // A dimension's lines, arrows and text are in an anonymous block, already in world coordinates.
            "DIMENSION" => self.expand(&r.name(2), m, layer, pen, ctx),
            // Attribute definitions show as ATTRIBs on the INSERT instead.
            "ATTDEF" => {}
            kind => w.skip(kind),
        }
    }

    /// Draws block `name` with transform `m`, as the child of an INSERT or DIMENSION.
    fn expand(&mut self, name: &str, m: Affine, layer: &'a str, pen: Pen, ctx: &Ctx<'a>) {
        if ctx.depth >= MAX_DEPTH {
            return;
        }
        let Some(block) = self.blocks.get(name) else { return };
        let inner = Ctx { m, layer: Some(layer), color: pen.color, depth: ctx.depth + 1 };
        for e in &block.entities {
            self.entity(e, &inner);
        }
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

/// A Catmull-Rom curve through `points`: close to, but not the same as, the cubic AutoCAD fits.
fn through(points: &[P]) -> Vec<P> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let at = |i: usize| points[i.min(points.len() - 1)];
    let mut out = Vec::new();
    for i in 0..points.len() - 1 {
        let (p0, p1, p2, p3) = (at(i.saturating_sub(1)), at(i), at(i + 1), at(i + 2));
        out.extend((0..SPLINE_STEPS).map(|s| {
            let t = s as f64 / SPLINE_STEPS as f64;
            let c = |k: usize| {
                let (a, b, c, d) = (p0[k], p1[k], p2[k], p3[k]);
                0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t + (3.0 * b - a - 3.0 * c + d) * t * t * t)
            };
            [c(0), c(1)]
        }));
    }
    out.push(points[points.len() - 1]);
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
    fn lists_entities_not_drawn() {
        let d = decode(&entities(&[(0, "HATCH"), (0, "HATCH"), (0, "IMAGE"), (0, "POINT"), (10, "0"), (20, "0")])).unwrap();
        assert_eq!(d.info.last().unwrap().1, "HATCH × 2, IMAGE × 1");
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
