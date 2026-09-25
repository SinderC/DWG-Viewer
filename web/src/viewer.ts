import type { DrawingDocument, View } from './formats/types';

/** Raster zoom limit, in device pixels per drawing pixel. */
const MAX_SCALE = 32;
/** Vector zoom limit, relative to fit. */
const MAX_VECTOR_ZOOM = 1000;
/** Share of the canvas a fitted vector drawing fills, so lines on its edges stay visible. */
const VECTOR_FIT = 0.95;
const ZOOM_STEP = 1.5;

/** Canvas viewer: mouse wheel zooms around the cursor, left-drag pans, double-click fits. */
export class Viewer {
  private readonly ctx: CanvasRenderingContext2D;
  private doc: DrawingDocument | null = null;
  private view: View = { scale: 1, x: 0, y: 0 };
  private drag: { x: number; y: number } | null = null;
  private inverted = false;
  private frame = 0;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly onViewChange: (view: View) => void,
  ) {
    this.ctx = canvas.getContext('2d')!;
    new ResizeObserver(() => {
      if (this.syncSize()) this.redraw();
    }).observe(canvas);
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', () => this.endDrag());
    canvas.addEventListener('pointercancel', () => this.endDrag());
    canvas.addEventListener('dblclick', () => this.fit());
  }

  setDocument(doc: DrawingDocument | null): void {
    this.doc?.free();
    this.doc = doc;
    this.fit();
  }

  fit(): void {
    this.syncSize();
    if (!this.doc) return this.redraw();
    const scale = this.fitScale();
    const { width: w, height: h } = this.canvas;
    this.setView({ scale, x: (this.doc.width - w / scale) / 2, y: (this.doc.height - h / scale) / 2 });
  }

  /** Swaps black and white. Returns the new state. */
  toggleInvert(): boolean {
    this.inverted = !this.inverted;
    this.redraw();
    return this.inverted;
  }

  /** One drawing pixel per device pixel, keeping the canvas centre fixed. Raster only. */
  actualSize(): void {
    this.zoomAt(1 / this.view.scale, this.canvas.width / 2, this.canvas.height / 2);
  }

  zoomIn(): void {
    this.zoomAt(ZOOM_STEP, this.canvas.width / 2, this.canvas.height / 2);
  }

  zoomOut(): void {
    this.zoomAt(1 / ZOOM_STEP, this.canvas.width / 2, this.canvas.height / 2);
  }

  /** Multiplies the scale by `factor`, keeping the drawing point under device pixel (px, py) fixed. */
  private zoomAt(factor: number, px: number, py: number): void {
    if (!this.doc) return;
    const { scale, x, y } = this.view;
    const max = this.doc.kind === 'raster' ? MAX_SCALE : this.fitScale() * MAX_VECTOR_ZOOM;
    const next = Math.min(max, Math.max(this.fitScale() / 4, scale * factor));
    this.setView({ scale: next, x: x + px / scale - px / next, y: y + py / scale - py / next });
  }

  private fitScale(): number {
    if (!this.doc) return 1;
    const fill = this.doc.kind === 'vector' ? VECTOR_FIT : 1;
    return fill * Math.min(this.canvas.width / this.doc.width, this.canvas.height / this.doc.height);
  }

  private setView(view: View): void {
    this.view = view;
    this.onViewChange(view);
    this.redraw();
  }

  redraw(): void {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      const { width, height } = this.canvas;
      if (this.doc?.kind === 'vector') {
        this.doc.draw(this.ctx, this.view, this.inverted);
      } else if (this.doc && width > 0 && height > 0) {
        this.ctx.putImageData(this.doc.render(this.view, width, height, this.inverted), 0, 0);
      } else {
        this.ctx.clearRect(0, 0, width, height);
      }
    });
  }

  /** Matches the canvas' backing store to its CSS size in device pixels. Returns true if it changed. */
  private syncSize(): boolean {
    const dpr = window.devicePixelRatio || 1;
    const width = Math.round(this.canvas.clientWidth * dpr);
    const height = Math.round(this.canvas.clientHeight * dpr);
    if (width === this.canvas.width && height === this.canvas.height) return false;
    this.canvas.width = width;
    this.canvas.height = height;
    return true;
  }

  private devicePoint(e: MouseEvent): [number, number] {
    const rect = this.canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    return [(e.clientX - rect.left) * dpr, (e.clientY - rect.top) * dpr];
  }

  private onWheel(e: WheelEvent): void {
    e.preventDefault();
    const pixels = e.deltaMode === WheelEvent.DOM_DELTA_LINE ? e.deltaY * 16 : e.deltaY;
    // Trackpad pinch arrives as a wheel event with ctrlKey and small deltas.
    const factor = Math.exp(-pixels * (e.ctrlKey ? 0.01 : 0.002));
    this.zoomAt(factor, ...this.devicePoint(e));
  }

  private onPointerDown(e: PointerEvent): void {
    if (e.button !== 0 || !this.doc) return;
    this.canvas.setPointerCapture(e.pointerId);
    this.canvas.classList.add('dragging');
    this.drag = { x: e.clientX, y: e.clientY };
  }

  private onPointerMove(e: PointerEvent): void {
    if (!this.drag) return;
    const dpr = window.devicePixelRatio || 1;
    const { scale, x, y } = this.view;
    const dx = ((e.clientX - this.drag.x) * dpr) / scale;
    const dy = ((e.clientY - this.drag.y) * dpr) / scale;
    this.drag = { x: e.clientX, y: e.clientY };
    this.setView({ scale, x: x - dx, y: y - dy });
  }

  private endDrag(): void {
    this.drag = null;
    this.canvas.classList.remove('dragging');
  }
}
