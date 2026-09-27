import type { FormatPlugin, Layer, VectorDocument, View } from './types';
import { DxfDoc, dxfBackground, dxfForeground, infoRows, RasterDoc, renderRaster } from './wasm';

const FOREGROUND = dxfForeground();
const BACKGROUND = dxfBackground();
/** DXF text height is the cap height; CSS font size is the em. */
const EM_PER_CAP = 1 / 0.7;
/** MTEXT default line spacing, in cap heights. */
const LINE_SPACING = 5 / 3;
/** Text smaller than this many device pixels is not drawn. */
const MIN_TEXT_PX = 1.5;
/** Colours lighter (on white) or darker (on black) than this luma are adjusted to stay readable. */
const MAX_LUMA_ON_WHITE = 170;
const MIN_LUMA_ON_BLACK = 70;
const ALIGN: CanvasTextAlign[] = ['left', 'center', 'right'];
const BASELINE: CanvasTextBaseline[] = ['alphabetic', 'bottom', 'middle', 'top'];

interface Text {
  x: number;
  y: number;
  /** Canvas transform from device-pixel glyph space: baseline and downward vectors, per unit of height. */
  matrix: [number, number, number, number];
  /** Cap height in drawing units. */
  height: number;
  /** Rough radius around the anchor that contains the text, in drawing units, for culling. */
  reach: number;
  align: CanvasTextAlign;
  baseline: CanvasTextBaseline;
  /** Lines are shifted up by this many line pitches so the block sits on its anchor. */
  shift: number;
  lines: string[];
  color: number;
  layer: number;
}

interface Stroke {
  color: number;
  /** In drawing units; 0 is a hairline. */
  width: number;
  path: Path2D;
}

interface Fill {
  path: Path2D;
  color: number;
  layer: number;
  rule: CanvasFillRule;
}

/** A raster image in the drawing; `x`, `y` is its top-left corner and `px` the size of a pixel, in drawing units. */
interface Image {
  raster: RasterDoc;
  x: number;
  y: number;
  px: number;
  width: number;
  height: number;
  layer: number;
}

/** CSS colour for a DXF colour on white paper, or on black when `invert` is set. */
function css(color: number, invert: boolean): string {
  if (color === FOREGROUND) return invert ? '#fff' : '#000';
  if (color === BACKGROUND) return invert ? '#000' : '#fff';
  let rgb = [(color >> 16) & 255, (color >> 8) & 255, color & 255];
  const luma = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
  if (!invert && luma > MAX_LUMA_ON_WHITE) {
    rgb = rgb.map((c) => (c * MAX_LUMA_ON_WHITE) / luma);
  } else if (invert && luma < MIN_LUMA_ON_BLACK) {
    const t = (MIN_LUMA_ON_BLACK - luma) / (255 - luma);
    rgb = rgb.map((c) => c + (255 - c) * t);
  }
  return `rgb(${rgb.map(Math.round).join(' ')})`;
}

/** Builds one Path2D per layer, colour and width, so a frame is a handful of stroke() calls. */
function strokes(doc: DxfDoc): Map<number, Map<string, Stroke>> {
  const byLayer = new Map<number, Map<string, Stroke>>();
  const pathFor = (layer: number, color: number, width: number) => {
    let byStyle = byLayer.get(layer);
    if (!byStyle) byLayer.set(layer, (byStyle = new Map()));
    const key = `${color}/${width}`;
    let stroke = byStyle.get(key);
    if (!stroke) byStyle.set(key, (stroke = { color, width, path: new Path2D() }));
    return stroke.path;
  };

  const points = doc.pathPoints();
  const colors = doc.pathColors();
  const widths = doc.pathWidths();
  const layers = doc.pathLayers();
  let k = 0;
  doc.pathLengths().forEach((n, i) => {
    const path = pathFor(layers[i], colors[i], widths[i]);
    path.moveTo(points[k], points[k + 1]);
    for (let j = 1; j < n; j++) path.lineTo(points[k + 2 * j], points[k + 2 * j + 1]);
    k += 2 * n;
  });

  // Each arc is the unit circle from t0 to t1, mapped by [u v centre].
  const arcs = doc.arcs();
  const arcLayers = doc.arcLayers();
  const arcWidths = doc.arcWidths();
  doc.arcColors().forEach((color, i) => {
    const [cx, cy, ux, uy, vx, vy, t0, t1] = arcs.subarray(8 * i, 8 * i + 8);
    const unit = new Path2D();
    unit.ellipse(0, 0, 1, 1, 0, t0, t1, t1 < t0);
    pathFor(arcLayers[i], color, arcWidths[i]).addPath(unit, new DOMMatrix([ux, uy, vx, vy, cx, cy]));
  });
  return byLayer;
}

/** One Path2D per fill, its rings as subpaths, in file order. */
function fills(doc: DxfDoc): Fill[] {
  const points = doc.fillPoints();
  const lengths = doc.fillRingLengths();
  const colors = doc.fillColors();
  const layers = doc.fillLayers();
  const evenOdd = doc.fillEvenOdd();
  let ring = 0;
  let k = 0;
  return Array.from(doc.fillRingCounts(), (count, i) => {
    const path = new Path2D();
    for (const end = ring + count; ring < end; ring++) {
      const n = lengths[ring];
      path.moveTo(points[k], points[k + 1]);
      for (let j = 1; j < n; j++) path.lineTo(points[k + 2 * j], points[k + 2 * j + 1]);
      path.closePath();
      k += 2 * n;
    }
    return { path, color: colors[i], layer: layers[i], rule: evenOdd[i] ? 'evenodd' : 'nonzero' };
  });
}

/** Takes the raster images out of `doc`; they must be freed. */
function images(doc: DxfDoc): Image[] {
  const placements = doc.imagePlacements();
  return doc.takeImages().map((raster, i) => {
    const [x, y, px, layer] = placements.subarray(4 * i, 4 * i + 4);
    return { raster, x, y, px, layer, width: raster.width, height: raster.height };
  });
}

let scratch: HTMLCanvasElement | undefined;

/**
 * Draws the part of `image` that is on screen. White is transparent: the image is multiplied onto
 * white paper, or rendered inverted and screened onto black paper, so overlapping images compose.
 */
function drawImage(ctx: CanvasRenderingContext2D, image: Image, view: View, invert: boolean): void {
  const { scale, x, y } = view;
  const s = scale * image.px;
  const left = (image.x - x) * scale;
  const top = (image.y - y) * scale;
  // Whole device pixels whose centres are inside the image, so none is rendered as the raster's off-image background.
  const x0 = Math.max(0, Math.ceil(left));
  const y0 = Math.max(0, Math.ceil(top));
  const x1 = Math.min(ctx.canvas.width, Math.floor(left + image.width * s));
  const y1 = Math.min(ctx.canvas.height, Math.floor(top + image.height * s));
  if (x1 <= x0 || y1 <= y0) return;
  const pixels = renderRaster(image.raster, { scale: s, x: (x0 - left) / s, y: (y0 - top) / s }, x1 - x0, y1 - y0, invert);
  scratch ??= document.createElement('canvas');
  scratch.width = x1 - x0;
  scratch.height = y1 - y0;
  scratch.getContext('2d')!.putImageData(pixels, 0, 0);
  ctx.globalCompositeOperation = invert ? 'screen' : 'multiply';
  ctx.drawImage(scratch, x0, y0);
  ctx.globalCompositeOperation = 'source-over';
}

function texts(doc: DxfDoc): Text[] {
  const data = doc.texts();
  const colors = doc.textColors();
  const layers = doc.textLayers();
  return doc.textStrings().map((string, i) => {
    const [x, y, ax, ay, ux, uy, halign, valign] = data.subarray(8 * i, 8 * i + 8);
    const height = Math.hypot(ux, uy);
    const lines = string.split('\n');
    const longest = Math.max(...lines.map((l) => l.length));
    return {
      x,
      y,
      // Glyph y points down, opposite to `up`.
      matrix: [ax / height, ay / height, -ux / height, -uy / height],
      height,
      reach: (Math.hypot(ax, ay) * longest + height * lines.length * LINE_SPACING) * EM_PER_CAP,
      align: ALIGN[halign],
      baseline: BASELINE[valign],
      shift: valign === 3 ? 0 : valign === 2 ? (lines.length - 1) / 2 : lines.length - 1,
      lines,
      color: colors[i],
      layer: layers[i],
    };
  });
}

function layers(doc: DxfDoc): Layer[] {
  const colors = doc.layerColors();
  const visible = doc.layerVisible();
  return doc.layerNames().map((name, i) => ({ name, color: css(colors[i], false), visible: visible[i] === 1 }));
}


/**
 * Copies everything the renderer needs out of `doc`, then frees it: the WASM copy is never read
 * again. Raster images stay in WASM and are freed with the returned document.
 */
export function vectorDocument(doc: DxfDoc): VectorDocument {
  try {
    const pictures = images(doc);
    const areas = fills(doc);
    const paths = strokes(doc);
    const labels = texts(doc);
    const layerList = layers(doc);
    return {
      kind: 'vector',
      width: doc.width,
      height: doc.height,
      units: doc.units,
      pageCount: doc.pageCount,
      info: infoRows(doc.info()),
      layers: layerList,
      draw(ctx, view, invert) {
        const { scale, x, y } = view;
        const { width, height } = ctx.canvas;
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.fillStyle = invert ? '#000' : '#fff';
        ctx.fillRect(0, 0, width, height);
        for (const image of pictures) {
          if (layerList[image.layer].visible) drawImage(ctx, image, view, invert);
        }

        // Fills go under all lines, whatever their order in the file.
        ctx.setTransform(scale, 0, 0, scale, -x * scale, -y * scale);
        for (const fill of areas) {
          if (!layerList[fill.layer].visible) continue;
          ctx.fillStyle = css(fill.color, invert);
          ctx.fill(fill.path, fill.rule);
        }

        // Lines are at least one device pixel wide at any zoom; hairlines exactly that.
        ctx.lineCap = 'round';
        ctx.lineJoin = 'round';
        for (const [layer, byStyle] of paths) {
          if (!layerList[layer].visible) continue;
          for (const { color, width: lineWidth, path } of byStyle.values()) {
            ctx.strokeStyle = css(color, invert);
            ctx.lineWidth = Math.max(lineWidth, 1 / scale);
            ctx.stroke(path);
          }
        }

        for (const t of labels) {
          const px = t.height * scale;
          const sx = (t.x - x) * scale;
          const sy = (t.y - y) * scale;
          const reach = t.reach * scale;
          if (!layerList[t.layer].visible || px < MIN_TEXT_PX || sx < -reach || sy < -reach || sx > width + reach || sy > height + reach) continue;
          ctx.setTransform(...t.matrix, sx, sy);
          ctx.font = `${px * EM_PER_CAP}px sans-serif`;
          ctx.textAlign = t.align;
          ctx.textBaseline = t.baseline;
          ctx.fillStyle = css(t.color, invert);
          t.lines.forEach((line, i) => ctx.fillText(line, 0, (i - t.shift) * px * LINE_SPACING));
        }
      },
      free: () => pictures.forEach((image) => image.raster.free()),
    };
  } finally {
    doc.free();
  }
}

export const dxf: FormatPlugin = {
  name: 'DXF',
  extensions: ['dxf'],

  // ASCII DXF starts with a `0 / SECTION` group, possibly after `999` comments. Binary DXF is
  // claimed too, so the user gets a clear error instead of "unsupported format".
  sniff(bytes) {
    const head = new TextDecoder('latin1').decode(bytes.subarray(0, 4096));
    return head.startsWith('AutoCAD Binary DXF') || /^(\s*999\r?\n.*\r?\n)*\s*0\r?\n\s*SECTION\s*\r?\n/.test(head);
  },

  open: (bytes) => vectorDocument(DxfDoc.open(bytes)),
};

export const dwg: FormatPlugin = {
  name: 'DWG',
  extensions: ['dwg'],
  // Every release starts with its version code: AC1012 (R13) … AC1032 (2018). Older ones are
  // claimed too, so the user gets a clear error instead of "unsupported format".
  sniff: (bytes) => /^AC1\d{3}$/.test(new TextDecoder('latin1').decode(bytes.subarray(0, 6))),
  open: (bytes) => vectorDocument(DxfDoc.openDwg(bytes)),
};
