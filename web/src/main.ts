import { cals } from './formats/cals';
import { cgm } from './formats/cgm';
import { dwf } from './formats/dwf';
import { dwg, dxf } from './formats/dxf';
import { hpgl } from './formats/hpgl';
import { tiff } from './formats/tiff';
import type { DrawingDocument, FormatPlugin, Layer, SnapPoint, TextHit, View } from './formats/types';
import { Minimap } from './minimap';
import { PAPERS, Viewer } from './viewer';

// HP-GL has no signature, so its loose sniff goes last.
const plugins: FormatPlugin[] = [dxf, dwg, dwf, cals, tiff, cgm, hpgl];

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const fileInput = $<HTMLInputElement>('file');
const title = $('title');
const toastEl = $('toast');
const toastText = toastEl.querySelector('span')!;
const zoomLabel = $('zoom');
const start = $('start');
const pager = $('pager');
const pageLabel = $('page');
const actualSize = $('actual');
const infoDialog = $<HTMLDialogElement>('info-dialog');
const infoBody = $('info-body');
const layersButton = $<HTMLButtonElement>('layers');
const layersPanel = $('layers-panel');
const layerFilter = $<HTMLInputElement>('layer-filter');
const layerList = $('layer-list');
const layerCount = $('layer-count');
const coords = $('coords');
const canvas = $<HTMLCanvasElement>('canvas');

fileInput.accept = plugins.flatMap((p) => p.extensions.map((e) => `.${e}`)).join(',');
$('formats').replaceChildren(
  ...plugins.map((p) => {
    const item = document.createElement('li');
    item.textContent = p.name;
    item.appendChild(document.createElement('span')).textContent = p.extensions.map((e) => `.${e}`).join(' ');
    return item;
  }),
);

const measureButton = $('measure');
const measureBar = $('measure-bar');
const measureKeep = $('measure-keep');
const toolButtons = { distance: $('measure-distance'), angle: $('measure-angle') };

type Point = { x: number; y: number };
type Tool = 'distance' | 'angle';
/** Points in drawing coordinates: two for a distance; arm, vertex and arm for an angle. */
type Measurement = { kind: Tool; points: Point[] };
const POINTS: Record<Tool, number> = { distance: 2, angle: 3 };

/**
 * Measure tool: the tool in use, the points placed so far, the pointer while placing the next one and
 * the drawing point it snapped to. With `keep`, finished measurements go to `kept` and stay on screen.
 */
const measure = {
  tool: null as Tool | null,
  keep: false,
  kept: [] as Measurement[],
  points: [] as Point[],
  hover: null as Point | null,
  snapped: null as SnapPoint | null,
};

const ACCENT = '#0a84ff';
/** Pointer distance, in CSS pixels, within which a drawing point is snapped to. */
const SNAP_PX = 10;
/** Lines within this many degrees of horizontal or vertical are straightened. */
const AXIS_SNAP_DEG = 4;
/** Radius of the arc marking an angle, in CSS pixels. */
const ANGLE_ARC_PX = 28;

function paintOverlay(ctx: CanvasRenderingContext2D, view: View): void {
  paintHits(ctx, view);
  if (!current) return;
  const dpr = window.devicePixelRatio || 1;
  const screen = (p: Point): [number, number] => [(p.x - view.x) * view.scale, (p.y - view.y) * view.scale];
  const live = measure.tool && { kind: measure.tool, points: measure.hover ? [...measure.points, measure.hover] : measure.points };
  for (const m of live ? [...measure.kept, live] : measure.kept) paintMeasurement(ctx, current.doc, m.kind, m.points.map(screen), m.points, dpr);
  if (measure.tool && measure.snapped) snapMarker(ctx, measure.snapped.kind, ...screen(measure.snapped), dpr);
}

/** Draws a measurement, finished or in progress, from its points on screen (`s`) and in the drawing (`p`). */
function paintMeasurement(ctx: CanvasRenderingContext2D, doc: DrawingDocument, kind: Tool, s: [number, number][], p: Point[], dpr: number): void {
  if (!s.length) return;
  ctx.lineWidth = 2 * dpr;
  ctx.strokeStyle = ACCENT;
  ctx.beginPath();
  s.forEach(([x, y]) => ctx.lineTo(x, y));
  ctx.stroke();
  if (kind === 'angle' && s.length === 3) {
    // The arc sweeps the inner angle, from arm a to arm c about vertex b.
    const [[ax, ay], [bx, by], [cx, cy]] = s;
    const [ta, tc] = [Math.atan2(ay - by, ax - bx), Math.atan2(cy - by, cx - bx)];
    const sweep = Math.atan2(Math.sin(tc - ta), Math.cos(tc - ta));
    const r = ANGLE_ARC_PX * dpr;
    ctx.beginPath();
    ctx.arc(bx, by, r, ta, ta + sweep, sweep < 0);
    ctx.stroke();
    const mid = ta + sweep / 2;
    label(ctx, `${number((Math.abs(sweep) * 180) / Math.PI, 4)}°`, bx + Math.cos(mid) * (r + 24 * dpr), by + Math.sin(mid) * (r + 24 * dpr), dpr);
  } else if (kind === 'distance' && s.length === 2) {
    label(ctx, distanceText(doc, p[0], p[1]), (s[0][0] + s[1][0]) / 2, (s[0][1] + s[1][1]) / 2, dpr);
  }
  ctx.fillStyle = '#fff';
  for (const [x, y] of s) dot(ctx, x, y, dpr);
}

function dot(ctx: CanvasRenderingContext2D, x: number, y: number, dpr: number): void {
  ctx.beginPath();
  ctx.arc(x, y, 4 * dpr, 0, 2 * Math.PI);
  ctx.fill();
  ctx.stroke();
}

/** The kind of a snapped point, as AutoCAD marks it: a square for an end, a triangle for a midpoint, a circle for a centre. */
function snapMarker(ctx: CanvasRenderingContext2D, kind: SnapPoint['kind'], x: number, y: number, dpr: number): void {
  const r = 7 * dpr;
  ctx.lineWidth = 2 * dpr;
  ctx.strokeStyle = ACCENT;
  ctx.beginPath();
  if (kind === 'end') ctx.rect(x - r, y - r, 2 * r, 2 * r);
  else if (kind === 'centre') ctx.arc(x, y, r, 0, 2 * Math.PI);
  else {
    ctx.moveTo(x, y - r);
    ctx.lineTo(x + r, y + r * 0.8);
    ctx.lineTo(x - r, y + r * 0.8);
    ctx.closePath();
  }
  ctx.stroke();
}

/** A pill-shaped label centred on (x, y), in device pixels. */
function label(ctx: CanvasRenderingContext2D, text: string, x: number, y: number, dpr: number): void {
  ctx.font = `600 ${12 * dpr}px system-ui, sans-serif`;
  const w = ctx.measureText(text).width + 16 * dpr;
  const h = 22 * dpr;
  ctx.fillStyle = ACCENT;
  ctx.beginPath();
  ctx.roundRect(x - w / 2, y - h / 2, w, h, h / 2);
  ctx.fill();
  ctx.fillStyle = '#fff';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(text, x, y);
}

/** Length and angle of a → b in the document's units; angles count counter-clockwise from +X. */
function distanceText(doc: DrawingDocument, a: Point, b: Point): string {
  const dx = b.x - a.x;
  const dy = a.y - b.y;
  const length = Math.hypot(dx, dy);
  const angle = `${(((Math.atan2(dy, dx) * 180) / Math.PI + 360) % 360).toFixed(1)}°`;
  if (doc.kind === 'vector') return `${number(length)} ${doc.units}`.trim() + ` · ${angle}`;
  const px = `${number(length, 4)} px`;
  return doc.dpi ? `${px} · ${number((length / doc.dpi) * 25.4, 4)} mm · ${angle}` : `${px} · ${angle}`;
}

/**
 * The point to place for pointer position `p` (with `pixel`, a CSS pixel in drawing units): a drawing point
 * near it, or else on the horizontal or vertical through the last point placed if the line is nearly so.
 * Nothing snaps while Shift is held.
 */
function snap(p: Point, pixel: number, free: boolean): { point: Point; snapped: SnapPoint | null } {
  const doc = current?.doc;
  if (free || !doc) return { point: p, snapped: null };
  const snapped = doc.kind === 'vector' ? doc.snap(p.x, p.y, SNAP_PX * pixel) : null;
  if (snapped) return { point: snapped, snapped };
  const last = measure.points.at(-1);
  if (!last) return { point: p, snapped: null };
  const angle = (Math.abs(Math.atan2(p.y - last.y, p.x - last.x)) * 180) / Math.PI;
  const off = Math.min(angle, Math.abs(angle - 90), 180 - angle);
  if (off > AXIS_SNAP_DEG) return { point: p, snapped: null };
  const horizontal = off === Math.min(angle, 180 - angle);
  return { point: horizontal ? { x: p.x, y: last.y } : { x: last.x, y: p.y }, snapped: null };
}

/** Starts measuring with `tool`, or stops with null. Kept measurements stay either way. */
function setTool(tool: Tool | null): void {
  measure.tool = tool;
  measure.points = [];
  measure.hover = measure.snapped = null;
  measureButton.setAttribute('aria-pressed', String(!!tool));
  for (const [kind, button] of Object.entries(toolButtons)) button.setAttribute('aria-pressed', String(kind === tool));
  measureBar.classList.toggle('open', !!tool);
  measureBar.inert = !tool;
  canvas.classList.toggle('measuring', !!tool);
  viewer.redrawOverlay();
}

function setKeep(keep: boolean): void {
  measure.keep = keep;
  measureKeep.setAttribute('aria-pressed', String(keep));
  if (!keep) clearMeasurements();
}

function clearMeasurements(): void {
  measure.kept = [];
  viewer.redrawOverlay();
}

const findButton = $<HTMLButtonElement>('find');
const findBar = $('find-bar');
const findInput = $<HTMLInputElement>('find-input');
const findCount = $('find-count');

/** Text matches of the find bar, and the one shown. */
const find = { hits: [] as TextHit[], index: 0 };

/** Matches as highlighter marks; the current one outlined. */
function paintHits(ctx: CanvasRenderingContext2D, view: View): void {
  const dpr = window.devicePixelRatio || 1;
  find.hits.forEach((hit, i) => {
    ctx.beginPath();
    for (const [x, y] of hit.corners) ctx.lineTo((x - view.x) * view.scale, (y - view.y) * view.scale);
    ctx.closePath();
    ctx.fillStyle = i === find.index ? 'rgb(255 214 10 / 0.55)' : 'rgb(255 214 10 / 0.3)';
    ctx.fill();
    if (i !== find.index) return;
    ctx.lineWidth = 2 * dpr;
    ctx.strokeStyle = '#ff9f0a';
    ctx.stroke();
  });
}

/** Searches again; `go` moves to match `index`, otherwise only the counter and highlights update. */
function runFind(index = 0, go = true): void {
  const doc = current?.doc;
  find.hits = doc?.kind === 'vector' ? doc.findText(findInput.value.trim()) : [];
  showHit(index, go);
}

/** Moves to match `index` (wrapping around), unless not `go`, and updates the counter. */
function showHit(index = find.index, go = true): void {
  const n = find.hits.length;
  find.index = n ? (index + n) % n : 0;
  findCount.textContent = !findInput.value.trim() ? '' : n ? `${find.index + 1} of ${n}` : 'No match';
  const hit = find.hits[find.index];
  if (hit && go) {
    const xs = hit.corners.map((c) => c[0]);
    const ys = hit.corners.map((c) => c[1]);
    const [x, y] = [Math.min(...xs), Math.min(...ys)];
    viewer.showRegion(x, y, Math.max(...xs) - x, Math.max(...ys) - y);
  }
  viewer.redrawOverlay();
}

function toggleFind(open = !findBar.classList.contains('open')): void {
  findBar.classList.toggle('open', open);
  findBar.inert = !open;
  findButton.setAttribute('aria-pressed', String(open));
  if (open) {
    findInput.select();
    findInput.focus();
  } else {
    find.hits = [];
    findInput.blur();
    viewer.redrawOverlay();
  }
}

findButton.addEventListener('click', () => toggleFind());
findInput.addEventListener('input', () => runFind());
findInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') showHit(find.index + (e.shiftKey ? -1 : 1));
  else if (e.key === 'Escape') toggleFind(false);
  else return;
  e.preventDefault();
});
$('find-prev').addEventListener('click', () => showHit(find.index - 1));
$('find-next').addEventListener('click', () => showHit(find.index + 1));
$('find-close').addEventListener('click', () => toggleFind(false));

const viewer = new Viewer(canvas, $<HTMLCanvasElement>('overlay'), paintOverlay, (view) => {
  // Raster: image pixels per device pixel, so 100% is 1:1. Vector: relative to fit, as drawing units have no screen size.
  const zoom = viewer.document?.kind === 'vector' ? viewer.fitRatio() : view.scale;
  zoomLabel.textContent = `${Math.round(zoom * 100)}%`;
  minimap.update();
  hover();
});
const minimapEl = $('minimap');
const minimap = new Minimap(minimapEl, minimapEl.querySelector('canvas')!, viewer);

function findPlugin(name: string, bytes: Uint8Array): FormatPlugin | undefined {
  const ext = name.split('.').pop()?.toLowerCase() ?? '';
  return plugins.find((p) => p.sniff(bytes)) ?? plugins.find((p) => p.extensions.includes(ext));
}

interface OpenFile {
  name: string;
  bytes: Uint8Array;
  plugin: FormatPlugin;
}

type Rows = [string, string][];

/** The file on screen, kept so its other pages can be opened and its properties shown. */
let current: (OpenFile & { page: number; pageCount: number; info: Rows; formatInfo: Rows; doc: DrawingDocument }) | null = null;

const number = (n: number, digits = 6) => Number(n.toPrecision(digits)).toLocaleString('en-US');

function fileSize(bytes: number): string {
  const units = ['bytes', 'KB', 'MB', 'GB'];
  const i = Math.min(units.length - 1, Math.floor(Math.log(Math.max(bytes, 1)) / Math.log(1024)));
  const value = bytes / 1024 ** i;
  const short = value >= 100 ? Math.round(value) : number(value, 3);
  return i === 0 ? `${bytes} bytes` : `${short} ${units[i]} (${bytes.toLocaleString('en-US')} bytes)`;
}

/** General rows for the file information sheet; the format's own rows are `doc.info`. */
function fileInfo(file: OpenFile, doc: DrawingDocument, page: number): Rows {
  const rows: Rows = [
    ['File', file.name],
    ['File size', fileSize(file.bytes.length)],
    ['Format', file.plugin.name],
  ];
  if (doc.pageCount > 1) rows.push(['Page', `${page + 1} of ${doc.pageCount}`]);
  if (doc.kind === 'raster') {
    rows.push(['Size', `${doc.width} × ${doc.height} px`]);
    if (doc.dpi) {
      const mm = (px: number) => number((px / doc.dpi) * 25.4, 4);
      rows.push(['Printed size', `${mm(doc.width)} × ${mm(doc.height)} mm at ${doc.dpi} dpi`]);
    }
  } else {
    rows.push(['Extents', `${number(doc.width)} × ${number(doc.height)} ${doc.units || 'units'}`]);
  }
  return rows;
}

/** A table of label/value rows. Values come from the file: set them as text, never as HTML. */
function rowsTable(rows: [string | Node, string][]): HTMLTableElement {
  const table = document.createElement('table');
  for (const [label, value] of rows) {
    const row = table.appendChild(document.createElement('tr'));
    row.appendChild(document.createElement('th')).append(label);
    row.appendChild(document.createElement('td')).textContent = value;
  }
  return table;
}

function infoSection(heading: string, rows: Rows): HTMLElement[] {
  const h3 = document.createElement('h3');
  h3.textContent = heading;
  return [h3, rowsTable(rows)];
}

function showInfo(): void {
  if (!current) return;
  const sections = infoSection('General', current.info);
  if (current.formatInfo.length) sections.push(...infoSection(current.plugin.name, current.formatInfo));
  infoBody.replaceChildren(...sections);
  infoDialog.showModal();
}

let toastTimer = 0;

/** Shows `message` in the toast; it hides itself after `ms` unless `ms` is 0. */
function toast(message: string, { error = false, ms = 4000 } = {}): void {
  clearTimeout(toastTimer);
  toastText.textContent = message;
  toastEl.classList.toggle('error', error);
  toastEl.classList.add('show');
  if (ms) toastTimer = setTimeout(hideToast, ms);
}

function hideToast(): void {
  clearTimeout(toastTimer);
  toastEl.classList.remove('show');
}

/** Layer rows of the document on screen, with their checkboxes. */
let layerRows: { layer: Layer; row: HTMLLabelElement; box: HTMLInputElement }[] = [];

/** Shows only `layer`. */
function solo(layer: Layer): void {
  layerRows.forEach((r) => (r.layer.visible = r.layer === layer));
  syncLayers();
}

function showLayers(layers: Layer[]): void {
  layerRows = layers.map((layer) => {
    const row = document.createElement('label');
    const box = row.appendChild(document.createElement('input'));
    const swatch = row.appendChild(document.createElement('span'));
    const name = row.appendChild(document.createElement('span'));
    const only = row.appendChild(document.createElement('button'));
    row.className = 'layer';
    box.type = 'checkbox';
    box.checked = layer.visible;
    swatch.className = 'swatch';
    swatch.style.background = layer.color;
    name.className = 'name';
    // Layer names come from the file: set them as text, never as HTML.
    name.textContent = name.title = layer.name;
    only.textContent = 'Only';
    only.title = 'Show only this layer (Alt-click the checkbox)';
    box.addEventListener('click', (e) => {
      if (e.altKey) return solo(layer);
      layer.visible = box.checked;
      syncLayers();
    });
    only.addEventListener('click', (e) => {
      e.preventDefault();
      solo(layer);
    });
    return { layer, row, box };
  });
  layerList.replaceChildren(...layerRows.map((r) => r.row));
  filterLayers();
}

function syncLayers(): void {
  layerRows.forEach((r) => (r.box.checked = r.layer.visible));
  const on = layerRows.filter((r) => r.layer.visible).length;
  layerCount.textContent = `${on} of ${layerRows.length} shown`;
  viewer.redraw();
  minimap.invalidate();
  if (findBar.classList.contains('open')) runFind(find.index, false);
}

function filterLayers(): void {
  const query = layerFilter.value.trim().toLowerCase();
  layerRows.forEach((r) => (r.row.hidden = !r.layer.name.toLowerCase().includes(query)));
}

/** Turns the layers that match the filter on or off. */
function setLayers(visible: boolean): void {
  layerRows.filter((r) => !r.row.hidden).forEach((r) => (r.layer.visible = visible));
  syncLayers();
}

function toggleLayersPanel(open = !layersPanel.classList.contains('open')): void {
  layersPanel.classList.toggle('open', open);
  layersPanel.inert = !open;
  layersButton.setAttribute('aria-pressed', String(open));
}

/** Errors stay until clicked, or until the next message or file replaces them. */
function showError(name: string, err: unknown): void {
  toast(`${name}: ${err instanceof Error ? err.message : String(err)}`, { error: true, ms: 0 });
}

/** Shows page `page` of `file`; on failure the current document stays on screen. */
function showPage(file: OpenFile, page: number): void {
  try {
    const doc = file.plugin.open(file.bytes, page);
    // The viewer owns (and frees) the document from here, so a failure below cannot leak it.
    viewer.setDocument(doc);
    current = { ...file, page, pageCount: doc.pageCount, info: fileInfo(file, doc, page), formatInfo: doc.info, doc };
    title.textContent = title.title = file.name;
    document.title = `${file.name} – Open Drawing Viewer`;
    hideToast();
    pager.hidden = doc.pageCount < 2;
    actualSize.hidden = doc.kind !== 'raster';
    const layers = doc.kind === 'vector' ? doc.layers : [];
    layersButton.hidden = layers.length === 0;
    if (layersButton.hidden) toggleLayersPanel(false);
    showLayers(layers);
    syncLayers();
    measure.points = [];
    measure.hover = measure.snapped = null;
    measure.kept = [];
    findButton.hidden = gridButton.hidden = doc.kind !== 'vector';
    weightsButton.hidden = doc.kind !== 'vector' || !doc.weighted;
    if (findButton.hidden) toggleFind(false);
    pageLabel.textContent = `${page + 1} / ${doc.pageCount}`;
    start.hidden = true;
    document.body.classList.add('has-doc');
    minimap.invalidate();
    if (findBar.classList.contains('open')) runFind();
    updateToolbarEdges();
  } catch (err) {
    showError(file.name, err);
  }
}

function turnPage(delta: number): void {
  const page = (current?.page ?? 0) + delta;
  if (current && page >= 0 && page < current.pageCount) showPage(current, page);
}

/** Files above this size get a progress toast; decoding blocks the page, so it must be painted first. */
const SLOW_FILE = 1 << 20;

/** Bumped by each openFile, so a slow load finishing after a later one does not replace it. */
let loading = 0;

async function openFile(file: File): Promise<void> {
  const id = ++loading;
  try {
    if (file.size > SLOW_FILE) {
      toast(`Opening ${file.name}…`, { ms: 0 });
      await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    if (id !== loading) return;
    const plugin = findPlugin(file.name, bytes);
    if (!plugin) throw new Error('Unsupported file format');
    showPage({ name: file.name, bytes, plugin }, 0);
  } catch (err) {
    showError(file.name, err);
  }
}

/** Decimals that resolve `step` (the size of a screen pixel), within reason. */
const decimals = (step: number) => Math.min(6, Math.max(0, Math.ceil(-Math.log10(step))));

/** A drawing point in world coordinates, as text: X, Y with unit, and an optional physical size. */
function worldPoint(doc: DrawingDocument, x: number, y: number, pixel: number): { x: string; y: string; unit: string; extra?: string } {
  if (doc.kind === 'vector') {
    const d = decimals(pixel);
    return { x: (doc.origin[0] + x).toFixed(d), y: (doc.origin[1] - y).toFixed(d), unit: doc.units };
  }
  const mm = (px: number) => ((px / doc.dpi) * 25.4).toFixed(1);
  const extra = doc.dpi ? `${mm(x)}, ${mm(y)} mm` : undefined;
  return { x: Math.floor(x).toString(), y: Math.floor(y).toString(), unit: 'px', extra };
}

function showCoords(e: PointerEvent): void {
  if (!current) return;
  const { x, y, pixel } = viewer.toDrawing(e);
  const p = worldPoint(current.doc, x, y, pixel);
  const label = (text: string) => Object.assign(document.createElement('b'), { textContent: text });
  const tail = [p.unit, p.extra].filter(Boolean).join(' · ');
  coords.replaceChildren(label('X'), p.x, label('Y'), p.y, tail ? ` ${tail}` : '');
  coords.hidden = false;
}

/** The pointer's last position on the canvas and Shift, so the point being placed follows view changes and Shift. */
let pointer: { clientX: number; clientY: number; shiftKey: boolean } | null = null;

/** Moves the point being placed to the pointer, snapped unless Shift is down. */
function hover(): void {
  if (!measure.tool || !pointer) return;
  // A finished measurement that is not kept stays on screen until the next click.
  if (measure.points.length === POINTS[measure.tool]) return;
  const p = viewer.toDrawing(pointer);
  ({ point: measure.hover, snapped: measure.snapped } = snap(p, p.pixel, pointer.shiftKey));
  viewer.redrawOverlay();
}

canvas.addEventListener('pointermove', (e) => {
  showCoords(e);
  pointer = { clientX: e.clientX, clientY: e.clientY, shiftKey: e.shiftKey };
  hover();
});
canvas.addEventListener('pointerleave', () => {
  coords.hidden = true;
  pointer = null;
  measure.hover = measure.snapped = null;
  viewer.redrawOverlay();
});
for (const type of ['keydown', 'keyup'] as const) {
  document.addEventListener(type, (e) => {
    if (e.key === 'Shift' && pointer) {
      pointer.shiftKey = type === 'keydown';
      hover();
    }
  });
}
viewer.onTap = (e) => {
  const tool = measure.tool;
  if (!tool) return;
  if (measure.points.length === POINTS[tool]) measure.points = [];
  const p = viewer.toDrawing(e);
  measure.points.push(snap(p, p.pixel, e.shiftKey).point);
  measure.hover = measure.snapped = null;
  if (measure.points.length === POINTS[tool] && measure.keep) {
    measure.kept.push({ kind: tool, points: measure.points });
    measure.points = [];
  }
  viewer.redrawOverlay();
};
measureButton.addEventListener('click', () => setTool(measure.tool ? null : 'distance'));
toolButtons.distance.addEventListener('click', () => setTool('distance'));
toolButtons.angle.addEventListener('click', () => setTool('angle'));
measureKeep.addEventListener('click', () => setKeep(!measure.keep));
$('measure-clear').addEventListener('click', clearMeasurements);
$('measure-close').addEventListener('click', () => setTool(null));
// While measuring, a double click is two points, not a request to fit.
canvas.addEventListener('dblclick', () => {
  if (!measure.tool) viewer.fit();
});

/** Saves the view as shown, measurements included, as a PNG next to the file's name. */
function exportView(): void {
  if (!current) return;
  const image = document.createElement('canvas');
  image.width = canvas.width;
  image.height = canvas.height;
  const ctx = image.getContext('2d')!;
  ctx.drawImage(canvas, 0, 0);
  ctx.drawImage($<HTMLCanvasElement>('overlay'), 0, 0);
  const base = current.name.replace(/\.[^.]*$/, '');
  const name = `${base}${current.pageCount > 1 ? `-page${current.page + 1}` : ''}.png`;
  image.toBlob((blob) => {
    if (!blob) return showError(name, new Error('Could not create the image'));
    const link = Object.assign(document.createElement('a'), { href: URL.createObjectURL(blob), download: name });
    link.click();
    setTimeout(() => URL.revokeObjectURL(link.href), 1000);
    toast(`Saved ${name}`);
  }, 'image/png');
}

$('export').addEventListener('click', exportView);

fileInput.addEventListener('change', () => {
  const file = fileInput.files?.[0];
  if (file) openFile(file);
  fileInput.value = '';
});

$('open').addEventListener('click', () => fileInput.click());
$('start-open').addEventListener('click', () => fileInput.click());
toastEl.addEventListener('click', hideToast);
$('zoom-in').addEventListener('click', () => viewer.zoomIn());
$('zoom-out').addEventListener('click', () => viewer.zoomOut());
$('fit').addEventListener('click', () => viewer.fit());
actualSize.addEventListener('click', () => viewer.actualSize());
$('prev-page').addEventListener('click', () => turnPage(-1));
$('next-page').addEventListener('click', () => turnPage(1));
$('info').addEventListener('click', showInfo);
layersButton.addEventListener('click', () => toggleLayersPanel());
layerFilter.addEventListener('input', filterLayers);
$('layers-on').addEventListener('click', () => setLayers(true));
$('layers-off').addEventListener('click', () => setLayers(false));
// A click on the backdrop lands on the dialog itself; clicks on its content land on the form.
document.querySelectorAll('dialog').forEach((dialog) =>
  dialog.addEventListener('click', (e) => {
    if (e.target === dialog) dialog.close();
  }),
);
const invert = $('invert');
const nextPaper = () => {
  const paper = viewer.nextPaper();
  // Highlighted while it differs from the default, like the other view toggles.
  invert.setAttribute('aria-pressed', String(paper !== PAPERS[0]));
  toast(`${paper.name} paper`);
  minimap.invalidate();
};
invert.addEventListener('click', nextPaper);

const gridButton = $<HTMLButtonElement>('grid');
const toggleGrid = () => gridButton.setAttribute('aria-pressed', String(viewer.toggleGrid()));
gridButton.addEventListener('click', toggleGrid);

const weightsButton = $<HTMLButtonElement>('weights');
const toggleWeights = () => weightsButton.setAttribute('aria-pressed', String(viewer.toggleWeights()));
weightsButton.addEventListener('click', toggleWeights);

const mac = /Mac|iPhone|iPad/.test(navigator.userAgent);

interface Shortcut {
  /** `KeyboardEvent.key` values, lower case for letters; "Mod+" means Cmd on a Mac, Ctrl elsewhere. */
  keys: string[];
  label: string;
  run: () => void;
  /** Whether it applies now; by default, when a drawing is open. */
  when?: () => boolean;
  /** Toolbar button whose tooltip gets the first key. */
  button?: string;
}

const always = () => true;
const shortcuts: Shortcut[] = [
  { keys: ['Mod+o'], label: 'Open a drawing', run: () => fileInput.click(), when: always, button: 'open' },
  { keys: ['+', '='], label: 'Zoom in', run: () => viewer.zoomIn(), button: 'zoom-in' },
  { keys: ['-'], label: 'Zoom out', run: () => viewer.zoomOut(), button: 'zoom-out' },
  { keys: ['0'], label: 'Fit to window', run: () => viewer.fit(), button: 'fit' },
  { keys: ['1'], label: 'Actual pixels', run: () => viewer.actualSize(), when: () => !!current && !actualSize.hidden, button: 'actual' },
  { keys: ['i'], label: 'Paper: dark blue, white, black', run: nextPaper, button: 'invert' },
  { keys: ['g'], label: 'Show or hide the grid', run: toggleGrid, when: () => !gridButton.hidden, button: 'grid' },
  { keys: ['w'], label: 'Show or hide line weights', run: toggleWeights, when: () => !weightsButton.hidden, button: 'weights' },
  { keys: ['l'], label: 'Show or hide layers', run: () => toggleLayersPanel(), when: () => !layersButton.hidden, button: 'layers' },
  { keys: ['ArrowLeft', 'PageUp'], label: 'Previous page', run: () => turnPage(-1), button: 'prev-page' },
  { keys: ['ArrowRight', 'PageDown'], label: 'Next page', run: () => turnPage(1), button: 'next-page' },
  { keys: ['Mod+s'], label: 'Save view as PNG', run: exportView, button: 'export' },
  { keys: ['Mod+f'], label: 'Find text', run: () => toggleFind(true), when: () => !findButton.hidden, button: 'find' },
  { keys: ['m'], label: 'Measure distance', run: () => setTool(measure.tool === 'distance' ? null : 'distance'), button: 'measure' },
  { keys: ['a'], label: 'Measure angle', run: () => setTool(measure.tool === 'angle' ? null : 'angle'), button: 'measure-angle' },
  { keys: ['Escape'], label: 'Stop measuring', run: () => setTool(null), when: () => !!measure.tool },
  { keys: ['?'], label: 'Keyboard shortcuts', run: () => keysDialog.showModal(), when: always, button: 'keys' },
];

const KEY_NAMES: Record<string, string> = { Escape: 'Esc', ArrowLeft: '←', ArrowRight: '→', PageUp: 'Page Up', PageDown: 'Page Down' };

function keyName(key: string): string {
  const name = key.replace('Mod+', '');
  const shown = KEY_NAMES[name] ?? name.toUpperCase();
  return key.startsWith('Mod+') ? (mac ? `⌘${shown}` : `Ctrl+${shown}`) : shown;
}

const keysDialog = $<HTMLDialogElement>('keys-dialog');
$('keys-body').replaceChildren(
  rowsTable(
    shortcuts.map((s) => {
      const keys = document.createDocumentFragment();
      s.keys.forEach((k) => (keys.appendChild(document.createElement('kbd')).textContent = keyName(k)));
      return [keys, s.label];
    }),
  ),
);
for (const id of ['keys', 'start-keys']) $(id).addEventListener('click', () => keysDialog.showModal());
for (const s of shortcuts) {
  const button = s.button && document.getElementById(s.button);
  if (button) button.title += ` (${keyName(s.keys[0])})`;
}

document.addEventListener('keydown', (e) => {
  const target = e.target as HTMLElement;
  // Typing in a text field, or a sheet that is open, keeps its keys.
  if (e.altKey || target.matches('input[type=search], input[type=text], textarea') || document.querySelector('dialog[open]')) return;
  const mod = mac ? e.metaKey : e.ctrlKey;
  if (mac ? e.ctrlKey : e.metaKey) return;
  const key = (mod ? 'Mod+' : '') + (e.key.length === 1 ? e.key.toLowerCase() : e.key);
  const shortcut = shortcuts.find((s) => s.keys.includes(key));
  if (!shortcut || !(shortcut.when ?? (() => !!current))()) return;
  e.preventDefault();
  shortcut.run();
});

/** Elements the drag is over: each one entered is left again, so the page is left when this reaches 0. */
let dragDepth = 0;
document.addEventListener('dragenter', () => {
  dragDepth++;
  document.body.classList.add('drop');
});
document.addEventListener('dragover', (e) => e.preventDefault());
document.addEventListener('dragleave', () => {
  if (--dragDepth <= 0) {
    dragDepth = 0;
    document.body.classList.remove('drop');
  }
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  dragDepth = 0;
  document.body.classList.remove('drop');
  const file = e.dataTransfer?.files[0];
  if (file) openFile(file);
});

const toolbar = $('toolbar');

/** Fades the toolbar's edges where more buttons are scrolled out of view. */
function updateToolbarEdges(): void {
  const { scrollLeft, scrollWidth, clientWidth } = toolbar;
  toolbar.classList.toggle('more-start', scrollLeft > 1);
  toolbar.classList.toggle('more-end', scrollLeft + clientWidth < scrollWidth - 1);
}
toolbar.addEventListener('scroll', updateToolbarEdges, { passive: true });
new ResizeObserver(updateToolbarEdges).observe(toolbar);

// iOS Safari only shows :active press feedback once the page listens for touches.
document.addEventListener('touchstart', () => {}, { passive: true });
