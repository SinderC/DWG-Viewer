import { drawDocument, type Viewer } from './viewer';

/** Zoom, relative to fit, above which the overview appears. */
const SHOW_ABOVE = 1.5;
/** Largest overview size, in CSS pixels. */
const MAX_WIDTH = 200;
const MAX_HEIGHT = 150;

/**
 * Overview of the whole drawing with the visible part outlined. Click or drag in it to move there.
 * The drawing is rendered once into `thumb` and only recomposited as the view moves.
 */
export class Minimap {
  private readonly ctx: CanvasRenderingContext2D;
  private readonly thumb = document.createElement('canvas');
  private stale = true;
  /** Drawing units per minimap device pixel. */
  private unit = 1;
  /** The drag in progress: offset from the pointer to the view centre, in drawing units, and whether to spring. */
  private grab: { dx: number; dy: number; animate: boolean } | null = null;

  constructor(
    private readonly el: HTMLElement,
    private readonly canvas: HTMLCanvasElement,
    private readonly viewer: Viewer,
  ) {
    this.ctx = canvas.getContext('2d')!;
    canvas.addEventListener('pointerdown', (e) => {
      canvas.setPointerCapture(e.pointerId);
      // The outline drags from where it was grabbed; elsewhere, the view springs there and keeps following.
      const p = this.point(e);
      const { scale, x, y } = this.viewer.currentView;
      const [w, h] = this.viewer.size.map((n) => n / scale);
      const inside = p.x >= x && p.x <= x + w && p.y >= y && p.y <= y + h;
      this.grab = inside ? { dx: x + w / 2 - p.x, dy: y + h / 2 - p.y, animate: false } : { dx: 0, dy: 0, animate: true };
      this.moveTo(e);
    });
    canvas.addEventListener('pointermove', (e) => this.moveTo(e));
    canvas.addEventListener('pointerup', () => (this.grab = null));
    canvas.addEventListener('pointercancel', () => (this.grab = null));
  }

  /** The drawing itself changed (document, page, layers or inversion). */
  invalidate(): void {
    this.stale = true;
    this.update();
  }

  /** Call when the view changes. */
  update(): void {
    const doc = this.viewer.document;
    const show = !!doc && this.viewer.fitRatio() > SHOW_ABOVE;
    this.el.classList.toggle('show', show);
    if (!doc || !show) return;

    const dpr = window.devicePixelRatio || 1;
    const fit = Math.min(MAX_WIDTH / doc.width, MAX_HEIGHT / doc.height);
    const width = Math.max(1, Math.round(doc.width * fit * dpr));
    const height = Math.max(1, Math.round(doc.height * fit * dpr));
    if (this.stale || this.thumb.width !== width || this.thumb.height !== height) {
      this.render(width, height);
      this.canvas.width = width;
      this.canvas.height = height;
      this.canvas.style.width = `${width / dpr}px`;
      this.canvas.style.height = `${height / dpr}px`;
    }
    this.unit = doc.width / width;

    const { scale, x, y } = this.viewer.currentView;
    const [w, h] = this.viewer.size;
    const s = 1 / this.unit;
    this.ctx.drawImage(this.thumb, 0, 0);
    this.ctx.lineWidth = 2 * dpr;
    this.ctx.strokeStyle = '#0a84ff';
    this.ctx.fillStyle = 'rgb(10 132 255 / 0.12)';
    const rect: [number, number, number, number] = [x * s, y * s, (w / scale) * s, (h / scale) * s];
    this.ctx.fillRect(...rect);
    this.ctx.strokeRect(...rect);
  }

  private render(width: number, height: number): void {
    const doc = this.viewer.document!;
    this.thumb.width = width;
    this.thumb.height = height;
    const ctx = this.thumb.getContext('2d')!;
    const view = { scale: width / doc.width, x: 0, y: 0 };
    drawDocument(ctx, doc, view, width, height, this.viewer.paper);
    this.stale = false;
  }

  /** The drawing point under the pointer. */
  private point(e: PointerEvent): { x: number; y: number } {
    const rect = this.canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    return { x: (e.clientX - rect.left) * dpr * this.unit, y: (e.clientY - rect.top) * dpr * this.unit };
  }

  private moveTo(e: PointerEvent): void {
    if (!this.grab) return;
    const p = this.point(e);
    this.viewer.centreOn(p.x + this.grab.dx, p.y + this.grab.dy, this.grab.animate);
  }
}
