import type { FormatPlugin, Layer, VectorDocument } from './types';
import { DxfDoc, dxfForeground, infoRows } from './wasm';

const FOREGROUND = dxfForeground();
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

/** CSS colour for a DXF colour on white paper, or on black when `invert` is set. */
function css(color: number, invert: boolean): string {
  if (color === FOREGROUND) return invert ? '#fff' : '#000';
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

/** Builds one Path2D per layer and colour, so a frame is a handful of stroke() calls. */
function strokes(doc: DxfDoc): Map<number, Map<number, Path2D>> {
  const byLayer = new Map<number, Map<number, Path2D>>();
  const pathFor = (layer: number, color: number) => {
    let byColor = byLayer.get(layer);
    if (!byColor) byLayer.set(layer, (byColor = new Map()));
    let path = byColor.get(color);
    if (!path) byColor.set(color, (path = new Path2D()));
    return path;
  };

  const points = doc.pathPoints();
  const colors = doc.pathColors();
  const layers = doc.pathLayers();
  let k = 0;
  doc.pathLengths().forEach((n, i) => {
    const path = pathFor(layers[i], colors[i]);
    path.moveTo(points[k], points[k + 1]);
    for (let j = 1; j < n; j++) path.lineTo(points[k + 2 * j], points[k + 2 * j + 1]);
    k += 2 * n;
  });

  // Each arc is the unit circle from t0 to t1, mapped by [u v centre].
  const arcs = doc.arcs();
  const arcLayers = doc.arcLayers();
  doc.arcColors().forEach((color, i) => {
    const [cx, cy, ux, uy, vx, vy, t0, t1] = arcs.subarray(8 * i, 8 * i + 8);
    const unit = new Path2D();
    unit.ellipse(0, 0, 1, 1, 0, t0, t1, t1 < t0);
    pathFor(arcLayers[i], color).addPath(unit, new DOMMatrix([ux, uy, vx, vy, cx, cy]));
  });
  return byLayer;
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


function vectorDocument(doc: DxfDoc): VectorDocument {
  const paths = strokes(doc);
  const labels = texts(doc);
  const layerList = layers(doc);
  return {
    kind: 'vector',
    width: doc.width,
    height: doc.height,
    units: doc.units,
    pageCount: 1,
    info: infoRows(doc.info()),
    layers: layerList,
    draw(ctx, view, invert) {
      const { scale, x, y } = view;
      const { width, height } = ctx.canvas;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.fillStyle = invert ? '#000' : '#fff';
      ctx.fillRect(0, 0, width, height);

      // Hairlines: one device pixel wide at any zoom.
      ctx.setTransform(scale, 0, 0, scale, -x * scale, -y * scale);
      ctx.lineWidth = 1 / scale;
      ctx.lineCap = 'round';
      ctx.lineJoin = 'round';
      for (const [layer, byColor] of paths) {
        if (!layerList[layer].visible) continue;
        for (const [color, path] of byColor) {
          ctx.strokeStyle = css(color, invert);
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
    free: () => doc.free(),
  };
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
