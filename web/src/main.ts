import { cals } from './formats/cals';
import { dwf } from './formats/dwf';
import { dwg, dxf } from './formats/dxf';
import { hpgl } from './formats/hpgl';
import { tiff } from './formats/tiff';
import type { DrawingDocument, FormatPlugin, Layer } from './formats/types';
import { Viewer } from './viewer';

// HP-GL has no signature, so its loose sniff goes last.
const plugins: FormatPlugin[] = [cals, tiff, dxf, dwg, dwf, hpgl];

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const fileInput = $<HTMLInputElement>('file');
const title = $('title');
const toastEl = $('toast');
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

fileInput.accept = plugins.flatMap((p) => p.extensions.map((e) => `.${e}`)).join(',');
$('formats').replaceChildren(
  ...plugins.map((p) => {
    const item = document.createElement('li');
    item.textContent = p.name;
    item.appendChild(document.createElement('span')).textContent = p.extensions.map((e) => `.${e}`).join(' ');
    return item;
  }),
);

const viewer = new Viewer($<HTMLCanvasElement>('canvas'), (view) => {
  zoomLabel.textContent = `${Math.round(view.scale * 100)}%`;
});

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
let current: (OpenFile & { page: number; pageCount: number; info: Rows; formatInfo: Rows }) | null = null;

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
  toastEl.textContent = message;
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

function showError(name: string, err: unknown): void {
  toast(`${name}: ${err instanceof Error ? err.message : String(err)}`, { error: true, ms: 8000 });
}

/** Shows page `page` of `file`; on failure the current document stays on screen. */
function showPage(file: OpenFile, page: number): void {
  try {
    const doc = file.plugin.open(file.bytes, page);
    current = { ...file, page, pageCount: doc.pageCount, info: fileInfo(file, doc, page), formatInfo: doc.info };
    title.textContent = title.title = file.name;
    document.title = `${file.name} – Drawing Viewer`;
    hideToast();
    pager.hidden = doc.pageCount < 2;
    actualSize.hidden = doc.kind !== 'raster';
    const layers = doc.kind === 'vector' ? doc.layers : [];
    layersButton.hidden = layers.length === 0;
    if (layersButton.hidden) toggleLayersPanel(false);
    showLayers(layers);
    syncLayers();
    pageLabel.textContent = `${page + 1} / ${doc.pageCount}`;
    start.hidden = true;
    document.body.classList.add('has-doc');
    viewer.setDocument(doc);
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

async function openFile(file: File): Promise<void> {
  try {
    if (file.size > SLOW_FILE) {
      toast(`Opening ${file.name}…`, { ms: 0 });
      await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    const plugin = findPlugin(file.name, bytes);
    if (!plugin) throw new Error('Unsupported file format');
    showPage({ name: file.name, bytes, plugin }, 0);
  } catch (err) {
    showError(file.name, err);
  }
}

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
const toggleInvert = () => invert.setAttribute('aria-pressed', String(viewer.toggleInvert()));
invert.addEventListener('click', toggleInvert);

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
  { keys: ['i'], label: 'Swap black and white', run: toggleInvert, button: 'invert' },
  { keys: ['l'], label: 'Show or hide layers', run: () => toggleLayersPanel(), when: () => !layersButton.hidden, button: 'layers' },
  { keys: ['ArrowLeft', 'PageUp'], label: 'Previous page', run: () => turnPage(-1), button: 'prev-page' },
  { keys: ['ArrowRight', 'PageDown'], label: 'Next page', run: () => turnPage(1), button: 'next-page' },
  { keys: ['?'], label: 'Keyboard shortcuts', run: () => keysDialog.showModal(), when: always },
];

const KEY_NAMES: Record<string, string> = { ArrowLeft: '←', ArrowRight: '→', PageUp: 'Page Up', PageDown: 'Page Down' };

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
for (const s of shortcuts) {
  const button = s.button && document.getElementById(s.button);
  if (button) button.title += ` (${keyName(s.keys[0])})`;
}

document.addEventListener('keydown', (e) => {
  const target = e.target as HTMLElement;
  if (e.altKey || target.closest('input, dialog') || document.querySelector('dialog[open]')) return;
  const mod = mac ? e.metaKey : e.ctrlKey;
  if (mac ? e.ctrlKey : e.metaKey) return;
  const key = (mod ? 'Mod+' : '') + (e.key.length === 1 ? e.key.toLowerCase() : e.key);
  const shortcut = shortcuts.find((s) => s.keys.includes(key));
  if (!shortcut || !(shortcut.when ?? (() => !!current))()) return;
  e.preventDefault();
  shortcut.run();
});

document.addEventListener('dragover', (e) => {
  e.preventDefault();
  document.body.classList.add('drop');
});
document.addEventListener('dragleave', (e) => {
  if (!e.relatedTarget) document.body.classList.remove('drop');
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  document.body.classList.remove('drop');
  const file = e.dataTransfer?.files[0];
  if (file) openFile(file);
});
