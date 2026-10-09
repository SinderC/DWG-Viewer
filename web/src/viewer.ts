import type { DrawingDocument, View } from './formats/types';
import { Spring } from './spring';

/** Raster zoom limit, in device pixels per drawing pixel. */
const MAX_SCALE = 32;
/** Vector zoom limit, relative to fit. */
const MAX_VECTOR_ZOOM = 1000;
/** Share of the canvas a fitted vector drawing fills, so lines on its edges stay visible. */
const VECTOR_FIT = 0.95;
const ZOOM_STEP = 1.5;
/** Space kept clear for the floating toolbar when fitting, in CSS pixels. */
const FIT_TOP = 60;
/** Scroll-like deceleration per millisecond after a flick (Apple's normal rate). */
const DECELERATION = 0.998;
/** Flicks slower than this, in CSS pixels per second, just stop. */
const MIN_FLICK = 60;
/** Pointer samples older than this are ignored when measuring release velocity. */
const VELOCITY_WINDOW = 100;
/** A press that moves less than this, in CSS pixels, is a tap rather than a drag. */
const TAP_SLOP = 4;
/** Frames slower than this make view changes jump instead of animate. */
const SLOW_FRAME_MS = 25;

const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');

/** Advances an animation by `dt` seconds; returns false once it has finished. */
type Motion = (dt: number) => boolean;

/** Draws on the transparent overlay canvas, in device pixels; it has been cleared. */
export type OverlayPainter = (ctx: CanvasRenderingContext2D, view: View) => void;

/** Canvas viewer: mouse wheel zooms around the cursor, left-drag pans (with momentum), double-click fits. */
export class Viewer {
  /** Called for a click or tap that did not pan. */
  onTap: ((e: PointerEvent) => void) | null = null;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly overlayCtx: CanvasRenderingContext2D;
  /** Whether the next frame must repaint the document, or only the overlay. */
  private docDirty = false;
  /** Pointers down on the canvas, in CSS pixels. */
  private readonly pointers = new Map<number, { x: number; y: number }>();
  /** Finger distance and midpoint of a two-finger pinch in progress, in CSS pixels. */
  private pinch: { distance: number; x: number; y: number } | null = null;
  /** Where the press in progress started, to tell taps from drags. */
  private press: { x: number; y: number; moved: boolean } | null = null;
  private doc: DrawingDocument | null = null;
  private view: View = { scale: 1, x: 0, y: 0 };
  /** Recent pointer positions of the drag in progress, in CSS pixels, newest last. */
  private drag: { x: number; y: number; t: number }[] | null = null;
  private inverted = false;
  private frame = 0;
  private motion: Motion | null = null;
  /** The springs of a running view animation: log scale, then the drawing point at the canvas centre. */
  private springs: [Spring, Spring, Spring] | null = null;
  private lastTick = 0;
  private lastFrameMs = 0;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly overlay: HTMLCanvasElement,
    private readonly paintOverlay: OverlayPainter,
    private readonly onViewChange: (view: View) => void,
  ) {
    this.ctx = canvas.getContext('2d')!;
    this.overlayCtx = overlay.getContext('2d')!;
    new ResizeObserver(() => {
      if (this.syncSize()) this.redraw();
    }).observe(canvas);
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', (e) => this.onPointerUp(e));
    canvas.addEventListener('pointercancel', (e) => this.onPointerUp(e, true));
    canvas.addEventListener('dblclick', () => this.fit());
  }

  setDocument(doc: DrawingDocument | null): void {
    this.doc?.free();
    this.doc = doc;
    this.stop();
    this.syncSize();
    this.setView(this.fitView());
  }

  fit(): void {
    this.syncSize();
    if (this.doc) this.animateTo(this.fitView());
  }

  /** Swaps black and white. Returns the new state. */
  toggleInvert(): boolean {
    this.inverted = !this.inverted;
    this.redraw();
    return this.inverted;
  }

  /** One drawing pixel per device pixel, keeping the canvas centre fixed. Raster only. */
  actualSize(): void {
    this.zoomAt(1 / this.target().scale, ...this.centre(), true);
  }

  zoomIn(): void {
    this.zoomAt(ZOOM_STEP, ...this.centre(), true);
  }

  zoomOut(): void {
    this.zoomAt(1 / ZOOM_STEP, ...this.centre(), true);
  }

  private centre(): [number, number] {
    return [this.canvas.width / 2, this.canvas.height / 2];
  }

  /** The view being shown, or the one an animation in progress is heading for. */
  private target(): View {
    return this.springs ? this.fromParams(this.springs.map((s) => s.target)) : this.view;
  }

  /**
   * Multiplies the scale by `factor`, keeping the drawing point under device pixel (px, py) fixed.
   * Button zooms animate and compound on the target, so repeated clicks keep accelerating.
   */
  private zoomAt(factor: number, px: number, py: number, animate = false): void {
    if (!this.doc) return;
    const { scale, x, y } = animate ? this.target() : this.view;
    const max = this.doc.kind === 'raster' ? MAX_SCALE : this.fitScale() * MAX_VECTOR_ZOOM;
    const next = Math.min(max, Math.max(this.fitScale() / 4, scale * factor));
    const view = { scale: next, x: x + px / scale - px / next, y: y + py / scale - py / next };
    if (animate) this.animateTo(view);
    else {
      this.stop();
      this.setView(view);
    }
  }

  private fitScale(): number {
    if (!this.doc) return 1;
    const fill = this.doc.kind === 'vector' ? VECTOR_FIT : 1;
    const top = FIT_TOP * (window.devicePixelRatio || 1);
    const height = Math.max(1, this.canvas.height - top);
    return fill * Math.min(this.canvas.width / this.doc.width, height / this.doc.height);
  }

  /** The whole drawing, centred in the canvas area below the toolbar. */
  private fitView(): View {
    if (!this.doc) return this.view;
    const scale = this.fitScale();
    const { width: w, height: h } = this.canvas;
    const top = FIT_TOP * (window.devicePixelRatio || 1);
    return { scale, x: (this.doc.width - w / scale) / 2, y: this.doc.height / 2 - (h + top) / (2 * scale) };
  }

  /** Animation parameters of a view: log scale (so zoom feels even), then the drawing point at the canvas centre. */
  private params({ scale, x, y }: View): [number, number, number] {
    const [cx, cy] = this.centre();
    return [Math.log(scale), x + cx / scale, y + cy / scale];
  }

  private fromParams([logScale, x, y]: number[]): View {
    const scale = Math.exp(logScale);
    const [cx, cy] = this.centre();
    return { scale, x: x - cx / scale, y: y - cy / scale };
  }

  /**
   * Springs from the view on screen to `view`. An animation in progress is retargeted, keeping
   * its velocity. Jumps instead when the user prefers reduced motion or frames are too slow to animate.
   */
  animateTo(view: View): void {
    if (reducedMotion.matches || this.lastFrameMs > SLOW_FRAME_MS) {
      this.stop();
      return this.setView(view);
    }
    const targets = this.params(view);
    if (!this.springs) {
      this.springs = this.params(this.view).map((v) => new Spring(v)) as [Spring, Spring, Spring];
    }
    this.springs.forEach((s, i) => (s.target = targets[i]));
    const springs = this.springs;
    this.start((dt) => {
      springs.forEach((s) => s.step(dt));
      // Settled once off by less than a device pixel at the centre, and by under 0.1% in scale.
      const pixel = 0.5 / Math.exp(springs[0].target);
      const done = springs[0].settled(1e-3) && springs[1].settled(pixel) && springs[2].settled(pixel);
      this.setView(this.fromParams(springs.map((s) => s.value)));
      if (done) this.springs = null;
      return !done;
    });
  }

  /** Runs `motion` once per frame until it finishes or is stopped. */
  private start(motion: Motion): void {
    if (!this.motion) this.lastTick = performance.now();
    this.motion = motion;
    this.redraw();
  }

  /** Stops any animation or momentum where it is: the user has taken over. */
  private stop(): void {
    this.motion = null;
    this.springs = null;
  }

  private setView(view: View): void {
    this.view = view;
    this.onViewChange(view);
    this.redraw();
  }

  redraw(): void {
    this.docDirty = true;
    this.redrawOverlay();
  }

  /** Repaints only the overlay, in the next frame. */
  redrawOverlay(): void {
    if (this.frame) return;
    this.frame = requestAnimationFrame((now) => this.paint(now));
  }

  /** The view on screen; `x`, `y` and `scale` map drawing units to device pixels. */
  get currentView(): View {
    return this.view;
  }

  private paint(now: number): void {
    // `frame` stays set until the drawing is done, so view changes made here don't queue another frame.
    if (this.motion) {
      const dt = Math.min(0.05, Math.max(0, (now - this.lastTick) / 1000));
      this.lastTick = now;
      if (!this.motion(dt)) this.motion = null;
    }
    const { width, height } = this.canvas;
    if (this.docDirty) {
      this.docDirty = false;
      const started = performance.now();
      if (this.doc?.kind === 'vector') {
        this.doc.draw(this.ctx, this.view, this.inverted);
      } else if (this.doc && width > 0 && height > 0) {
        this.ctx.putImageData(this.doc.render(this.view, width, height, this.inverted), 0, 0);
      } else {
        this.ctx.clearRect(0, 0, width, height);
      }
      this.lastFrameMs = performance.now() - started;
    }
    this.overlayCtx.setTransform(1, 0, 0, 1, 0, 0);
    this.overlayCtx.clearRect(0, 0, width, height);
    if (this.doc) this.paintOverlay(this.overlayCtx, this.view);
    this.frame = 0;
    if (this.motion) this.redraw();
  }

  /** Matches the canvas' backing store to its CSS size in device pixels. Returns true if it changed. */
  private syncSize(): boolean {
    const dpr = window.devicePixelRatio || 1;
    const width = Math.round(this.canvas.clientWidth * dpr);
    const height = Math.round(this.canvas.clientHeight * dpr);
    if (width === this.canvas.width && height === this.canvas.height) return false;
    this.canvas.width = this.overlay.width = width;
    this.canvas.height = this.overlay.height = height;
    return true;
  }

  /** Drawing coordinates under the mouse, and the size of one CSS pixel in drawing units. */
  toDrawing(e: MouseEvent): { x: number; y: number; pixel: number } {
    const [px, py] = this.devicePoint(e);
    const { scale, x, y } = this.view;
    return { x: x + px / scale, y: y + py / scale, pixel: (window.devicePixelRatio || 1) / scale };
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
    // Grabbing the drawing catches it mid-flight.
    this.stop();
    this.canvas.setPointerCapture(e.pointerId);
    this.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (this.pointers.size === 2) {
      // A second finger turns the drag into a pinch; it is no longer a tap either.
      this.drag = this.press = null;
      this.pinch = this.pinchState();
      return;
    }
    if (this.pointers.size > 2) return;
    this.canvas.classList.add('dragging');
    this.drag = [{ x: e.clientX, y: e.clientY, t: e.timeStamp }];
    this.press = { x: e.clientX, y: e.clientY, moved: false };
  }

  private pinchState(): { distance: number; x: number; y: number } {
    const [a, b] = this.pointers.values();
    return { distance: Math.hypot(a.x - b.x, a.y - b.y), x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  }

  private onPointerMove(e: PointerEvent): void {
    if (!this.pointers.has(e.pointerId)) return;
    this.pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (this.pinch) return this.onPinch();
    if (!this.drag) return;
    if (this.press && Math.hypot(e.clientX - this.press.x, e.clientY - this.press.y) > TAP_SLOP) this.press.moved = true;
    // Until it is clearly a drag, the drawing stays put so a tap lands where it was aimed.
    if (this.press && !this.press.moved) return;
    const dpr = window.devicePixelRatio || 1;
    const { scale, x, y } = this.view;
    const last = this.drag[this.drag.length - 1];
    const dx = ((e.clientX - last.x) * dpr) / scale;
    const dy = ((e.clientY - last.y) * dpr) / scale;
    this.drag.push({ x: e.clientX, y: e.clientY, t: e.timeStamp });
    this.drag = this.drag.filter((p) => e.timeStamp - p.t <= VELOCITY_WINDOW);
    this.setView({ scale, x: x - dx, y: y - dy });
  }

  /** The drawing follows both fingers: it pans with their midpoint and scales with their distance. */
  private onPinch(): void {
    const before = this.pinch!;
    const now = this.pinchState();
    this.pinch = now;
    const dpr = window.devicePixelRatio || 1;
    const rect = this.canvas.getBoundingClientRect();
    const { scale, x, y } = this.view;
    this.view = { scale, x: x - ((now.x - before.x) * dpr) / scale, y: y - ((now.y - before.y) * dpr) / scale };
    this.zoomAt(now.distance / Math.max(1, before.distance), (now.x - rect.left) * dpr, (now.y - rect.top) * dpr);
  }

  private onPointerUp(e: PointerEvent, cancelled = false): void {
    if (!this.pointers.delete(e.pointerId)) return;
    if (this.pinch) {
      if (this.pointers.size >= 2) return void (this.pinch = this.pinchState());
      // Lifting one finger of a pinch hands over to dragging with the other, without a flick or tap.
      this.pinch = null;
      const rest = [...this.pointers.values()][0];
      if (rest) this.drag = [{ ...rest, t: e.timeStamp }];
      return;
    }
    if (this.pointers.size === 0) this.endDrag(cancelled ? undefined : e);
  }

  /** On release, the drawing keeps the finger's velocity and decelerates like a scroll view. */
  private endDrag(e?: PointerEvent): void {
    const samples = this.drag?.filter((p) => !e || e.timeStamp - p.t <= VELOCITY_WINDOW);
    const tap = e && this.press && !this.press.moved;
    this.drag = null;
    this.press = null;
    this.canvas.classList.remove('dragging');
    if (tap) return this.onTap?.(e);
    if (!e || !samples || samples.length < 2 || reducedMotion.matches) return;
    const first = samples[0];
    const last = samples[samples.length - 1];
    const seconds = (last.t - first.t) / 1000;
    if (seconds <= 0) return;
    let vx = (last.x - first.x) / seconds;
    let vy = (last.y - first.y) / seconds;
    if (Math.hypot(vx, vy) < MIN_FLICK) return;
    this.start((dt) => {
      const dpr = window.devicePixelRatio || 1;
      const { scale, x, y } = this.view;
      this.setView({ scale, x: x - (vx * dt * dpr) / scale, y: y - (vy * dt * dpr) / scale });
      const decay = DECELERATION ** (dt * 1000);
      vx *= decay;
      vy *= decay;
      return Math.hypot(vx, vy) >= MIN_FLICK / 4;
    });
  }
}
