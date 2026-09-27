import { cals } from './formats/cals';
import { dwg, dxf } from './formats/dxf';
import { hpgl } from './formats/hpgl';
import { tiff } from './formats/tiff';
import type { DrawingDocument, FormatPlugin, Layer } from './formats/types';
import { Viewer } from './viewer';

// HP-GL has no signature, so its loose sniff goes last.
const plugins: FormatPlugin[] = [cals, tiff, dxf, dwg, hpgl];

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const fileInput = $<HTMLInputElement>('file');
const status = $('status');
const zoomLabel = $('zoom');
const hint = $('hint');
const pager = $('pager');
const pageLabel = $('page');
const actualSize = $('actual');
const infoButton = $<HTMLButtonElement>('info');
const infoDialog = $<HTMLDialogElement>('info-dialog');
const infoTable = $<HTMLTableElement>('info-table');
const layersButton = $<HTMLButtonElement>('layers');
const layersPanel = $('layers-panel');
const layerFilter = $<HTMLInputElement>('layer-filter');
const layerList = $('layer-list');

fileInput.accept = plugins.flatMap((p) => p.extensions.map((e) => `.${e}`)).join(',');

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

/** The file on screen, kept so its other pages can be opened and its properties shown. */
let current: (OpenFile & { page: number; pageCount: number; info: [string, string][] }) | null = null;

const number = (n: number, digits = 6) => Number(n.toPrecision(digits)).toLocaleString('en-US');

function fileSize(bytes: number): string {
  const units = ['bytes', 'KB', 'MB', 'GB'];
  const i = Math.min(units.length - 1, Math.floor(Math.log(Math.max(bytes, 1)) / Math.log(1024)));
  const value = bytes / 1024 ** i;
  const short = value >= 100 ? Math.round(value) : number(value, 3);
  return i === 0 ? `${bytes} bytes` : `${short} ${units[i]} (${bytes.toLocaleString('en-US')} bytes)`;
}

/** Rows for the file information dialog: general properties, then the format's own. */
function fileInfo(file: OpenFile, doc: DrawingDocument, page: number): [string, string][] {
  const rows: [string, string][] = [
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
  return [...rows, ...doc.info];
}

function showInfo(): void {
  if (!current) return;
  // Values come from the file: set them as text, never as HTML.
  infoTable.replaceChildren(
    ...current.info.map(([label, value]) => {
      const row = document.createElement('tr');
      const th = row.appendChild(document.createElement('th'));
      const td = row.appendChild(document.createElement('td'));
      th.textContent = label;
      td.textContent = value;
      return row;
    }),
  );
  infoDialog.showModal();
}

/** Layer rows of the document on screen, with their checkboxes. */
let layerRows: { layer: Layer; row: HTMLLabelElement; box: HTMLInputElement }[] = [];

function showLayers(layers: Layer[]): void {
  layerRows = layers.map((layer) => {
    const row = document.createElement('label');
    const box = row.appendChild(document.createElement('input'));
    const swatch = row.appendChild(document.createElement('span'));
    const name = row.appendChild(document.createElement('span'));
    box.type = 'checkbox';
    box.checked = layer.visible;
    swatch.className = 'swatch';
    swatch.style.background = layer.color;
    // Layer names come from the file: set them as text, never as HTML.
    name.textContent = name.title = layer.name;
    box.addEventListener('click', (e) => {
      if (e.altKey) layerRows.forEach((r) => (r.layer.visible = r.layer === layer));
      else layer.visible = box.checked;
      syncLayers();
    });
    return { layer, row, box };
  });
  layerList.replaceChildren(...layerRows.map((r) => r.row));
  filterLayers();
}

function syncLayers(): void {
  layerRows.forEach((r) => (r.box.checked = r.layer.visible));
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

function toggleLayersPanel(open = layersPanel.hidden): void {
  layersPanel.hidden = !open;
  layersButton.setAttribute('aria-pressed', String(open));
}

function showError(name: string, err: unknown): void {
  status.textContent = `${name}: ${err instanceof Error ? err.message : String(err)}`;
  status.classList.add('error');
}

/** Shows page `page` of `file`; on failure the current document stays on screen. */
function showPage(file: OpenFile, page: number): void {
  try {
    const doc = file.plugin.open(file.bytes, page);
    current = { ...file, page, pageCount: doc.pageCount, info: fileInfo(file, doc, page) };
    status.textContent = file.name;
    status.classList.remove('error');
    pager.hidden = doc.pageCount < 2;
    actualSize.hidden = doc.kind !== 'raster';
    infoButton.disabled = false;
    const layers = doc.kind === 'vector' ? doc.layers : [];
    layersButton.hidden = layers.length === 0;
    if (layersButton.hidden) toggleLayersPanel(false);
    showLayers(layers);
    pageLabel.textContent = `${page + 1} / ${doc.pageCount}`;
    hint.hidden = true;
    viewer.setDocument(doc);
  } catch (err) {
    showError(file.name, err);
  }
}

function turnPage(delta: number): void {
  const page = (current?.page ?? 0) + delta;
  if (current && page >= 0 && page < current.pageCount) showPage(current, page);
}

async function openFile(file: File): Promise<void> {
  status.textContent = `Loading ${file.name}…`;
  status.classList.remove('error');
  try {
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

$('zoom-in').addEventListener('click', () => viewer.zoomIn());
$('zoom-out').addEventListener('click', () => viewer.zoomOut());
$('fit').addEventListener('click', () => viewer.fit());
actualSize.addEventListener('click', () => viewer.actualSize());
$('prev-page').addEventListener('click', () => turnPage(-1));
$('next-page').addEventListener('click', () => turnPage(1));
infoButton.addEventListener('click', showInfo);
layersButton.addEventListener('click', () => toggleLayersPanel());
layerFilter.addEventListener('input', filterLayers);
$('layers-on').addEventListener('click', () => setLayers(true));
$('layers-off').addEventListener('click', () => setLayers(false));
// A click on the backdrop lands on the dialog itself; clicks on its content land on the form.
infoDialog.addEventListener('click', (e) => {
  if (e.target === infoDialog) infoDialog.close();
});
const invert = $('invert');
invert.addEventListener('click', () => invert.setAttribute('aria-pressed', String(viewer.toggleInvert())));

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
