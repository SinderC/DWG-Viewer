import type { DrawingDocument, View } from './formats/types';
import { drawGrid } from './grid';
import { Spring } from './spring';

/** Raster zoom limit, in device pixels per drawing pixel. */
const MAX_SCALE = 32;
/** Vector zoom limit, relative to fit. */
const MAX_VECTOR_ZOOM = 1000;
/** Share of the canvas a fitted vector drawing fills, so lines on its edges stay visible. */
const VECTOR_FIT = 0.95;
const ZOOM_STEP = 1.5;
/** Space kept clear for the floating toolbar when fitting, in CSS pixels. */
const FIT_BOTTOM = 72;
/** Scroll-like deceleration per millisecond after a flick (Apple's normal rate). */
const DECELERATION = 0.998;
/** Flicks slower than this, in CSS pixels per second, just stop. */
const MIN_FLICK = 60;
/** Pointer samples older than this are ignored when measuring release velocity. */
const VELOCITY_WINDOW = 100;
/** A press that moves less than this, in CSS pixels, is a tap rather than a drag; fingers need more room. */
const TAP_SLOP = 4;
const TOUCH_TAP_SLOP = 10;
/** Apple's rubber-band constant: how strongly a pull past the limits is resisted. */
const RUBBER = 0.55;
/** Frames slower than this make view changes jump instead of animate. */
const SLOW_FRAME_MS = 25;

const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');

/** Paper colours, in toggle order, the first by default. A tinted paper is drawn inverted, then its black is lightened to `tint`. */
export const PAPERS: { name: string; invert: boolean; tint?: string }[] = [
  { name: 'Dark blue', invert: true, tint: '#212830' },
  { name: 'White', invert: false },
  { name: 'Black', invert: true },
];
export type Paper = (typeof PAPERS)[number];

/** Draws the `width` × `height` viewport of `doc` onto `ctx` on `paper`, with line weights if `weights`. */
export function drawDocument(ctx: CanvasRenderingContext2D, doc: DrawingDocument, view: View, width: number, height: number, paper: Paper, weights = false): void {
  if (doc.kind === 'vector') doc.draw(ctx, view, paper.invert, weights);
  else ctx.putImageData(doc.render(view, width, height, paper.invert), 0, 0);
  if (!paper.tint) return;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalCompositeOperation = 'lighten';
  ctx.fillStyle = paper.tint;
  ctx.fillRect(0, 0, width, height);
  ctx.globalCompositeOperation = 'source-over';
}

/** Advances an animation by `dt` seconds; returns false once it has finished. */
type Motion = (dt: number) => boolean;

/** Draws on the transparent overlay canvas, in device pixels; it has been cleared. */
export type OverlayPainter = (ctx: CanvasRenderingContext2D, view: View) => void;

/**
 * Past [lo, hi], `value` follows ever less, never more than `dim` beyond the edge, like a rubber band.
 * `inverse` undoes it: from where the value is shown to where it was pulled.
 */
function rubberband(value: number, [lo, hi]: [number, number], dim: number, inverse: boolean): number {
  const edge = Math.min(hi, Math.max(lo, value));
  const over = Math.abs(value - edge);
  if (!over) return value;
  const shown = Math.min(over, dim * 0.99);
  const moved = inverse ? (shown * dim) / (RUBBER * (dim - shown)) : (over * dim * RUBBER) / (dim + RUBBER * over);
  return edge + Math.sign(value - edge) * moved;
}

/** `view` at scale `next`, keeping the drawing point under device pixel (px, py) fixed. */
function zoomedAt({ scale, x, y }: View, next: number, px: number, py: number): View {
  return { scale: next, x: x + px / scale - px / next, y: y + py / scale - py / next };
}

/**
 * Canvas viewer: the mouse wheel and pinches zoom around the pointer; dragging and trackpad scrolling
 * pan. Drags and pinches may pull past the limits (see `limits`), and spring back on release.
 */
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
  private press: { x: number; y: number; slop: number; moved: boolean } | null = null;
  /** Where the pointers hold the view, ignoring the limits; the view shown is this rubber-banded. */
  private held: View | null = null;
  private doc: DrawingDocument | null = null;
  private view: View = { scale: 1, x: 0, y: 0 };
  /** Recent pointer positions of the drag in progress, in CSS pixels, newest last. */
  private drag: { x: number; y: number; t: number }[] | null = null;
  private paperIndex = 0;
  private grid = false;
  private weights = false;
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
      // The fitted scale depends on the canvas size, so listeners that show zoom relative to fit must hear of it.
      if (this.syncSize()) this.setView(this.view);
    }).observe(canvas);
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', (e) => this.onPointerUp(e));
    canvas.addEventListener('pointercancel', (e) => this.onPointerUp(e, true));
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

  /** Switches to the next paper colour, wrapping round. Returns the new paper. */
  nextPaper(): Paper {
    this.paperIndex = (this.paperIndex + 1) % PAPERS.length;
    this.redraw();
    return this.paper;
  }

  /** Shows line weights on vector drawings, or draws every line thin. Returns the new state. */
  toggleWeights(): boolean {
    this.weights = !this.weights;
    this.redraw();
    return this.weights;
  }

  /** Shows or hides the grid on vector drawings. Returns the new state. */
  toggleGrid(): boolean {
    this.grid = !this.grid;
    this.redraw();
    return this.grid;
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
    const view = animate ? this.target() : this.view;
    const next = Math.min(this.maxScale(), Math.max(this.fitScale() / 4, view.scale * factor));
    this.go(zoomedAt(view, next, px, py), animate);
  }

  private maxScale(): number {
    return this.doc?.kind === 'raster' ? MAX_SCALE : this.fitScale() * MAX_VECTOR_ZOOM;
  }

  private scaleLimits(): [number, number] {
    return [Math.log(this.fitScale() / 4), Math.log(this.maxScale())];
  }

  /**
   * Range of the drawing point at the canvas centre (see `params`), at `scale`. A drawing that fits the
   * canvas stays centred above the toolbar, as fitted; a larger one may bring its edges in, clear of the
   * floating chrome, by up to a quarter of the canvas.
   */
  private centreLimits(scale: number): [number, number][] {
    const { width, height } = this.doc!;
    const bottom = FIT_BOTTOM * (window.devicePixelRatio || 1);
    const range = (size: number, visible: number, mid: number): [number, number] => {
      const margin = Math.min(visible / 4, Math.max(0, size - visible) / 2);
      return [Math.min(mid, visible / 2 - margin), Math.max(mid, size - visible / 2 + margin)];
    };
    const w = this.canvas.width / scale;
    const h = this.canvas.height / scale;
    return [range(width, w, width / 2), range(height, h, height / 2 + bottom / (2 * scale))];
  }

  /** `view` brought within the limits. */
  private clamp(view: View): View {
    if (!this.doc) return view;
    const within = (v: number, [lo, hi]: [number, number]) => Math.min(hi, Math.max(lo, v));
    const [logScale, cx, cy] = this.params(view);
    const clamped = within(logScale, this.scaleLimits());
    const [xs, ys] = this.centreLimits(Math.exp(clamped));
    return this.fromParams([clamped, within(cx, xs), within(cy, ys)]);
  }

  private inBounds(view: View): boolean {
    if (!this.doc) return true;
    const limits = [this.scaleLimits(), ...this.centreLimits(view.scale)];
    return this.params(view).every((v, i) => v >= limits[i][0] - 1e-6 && v <= limits[i][1] + 1e-6);
  }

  /**
   * The view shown for one held past the limits: zoom resists beyond doubling, panning beyond a canvas
   * width or height. `inverse` turns a view on screen back into where it is held.
   */
  private band(view: View, inverse = false): View {
    if (!this.doc) return view;
    const p = this.params(view);
    p[0] = rubberband(p[0], this.scaleLimits(), Math.LN2, inverse);
    const shown = inverse ? view.scale : Math.exp(p[0]);
    const [xs, ys] = this.centreLimits(shown);
    p[1] = rubberband(p[1], xs, this.canvas.width / shown, inverse);
    p[2] = rubberband(p[2], ys, this.canvas.height / shown, inverse);
    return this.fromParams(p);
  }

  /** Springs back within the limits, carrying a flick's velocity (CSS pixels per second) into the bounce. */
  private settle(vx = 0, vy = 0): void {
    this.animateTo(this.clamp(this.view));
    if (!this.springs) return;
    const dpr = window.devicePixelRatio || 1;
    this.springs[1].velocity = (-vx * dpr) / this.view.scale;
    this.springs[2].velocity = (-vy * dpr) / this.view.scale;
  }

  private fitScale(): number {
    if (!this.doc) return 1;
    const fill = this.doc.kind === 'vector' ? VECTOR_FIT : 1;
    const bottom = FIT_BOTTOM * (window.devicePixelRatio || 1);
    const height = Math.max(1, this.canvas.height - bottom);
    return fill * Math.min(this.canvas.width / this.doc.width, height / this.doc.height);
  }

  /** The whole drawing, centred in the canvas area above the toolbar. */
  private fitView(): View {
    if (!this.doc) return this.view;
    const scale = this.fitScale();
    const { width: w, height: h } = this.canvas;
    const bottom = FIT_BOTTOM * (window.devicePixelRatio || 1);
    return { scale, x: (this.doc.width - w / scale) / 2, y: this.doc.height / 2 - (h - bottom) / (2 * scale) };
  }

  /** Shows `view` within the limits, springing to it or jumping there (which stops any animation in progress). */
  private go(view: View, animate: boolean): void {
    if (animate) return this.animateTo(this.clamp(view));
    this.stop();
    this.setView(this.clamp(view));
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

  get document(): DrawingDocument | null {
    return this.doc;
  }

  get paper(): Paper {
    return PAPERS[this.paperIndex];
  }

  /** Canvas size in device pixels. */
  get size(): [number, number] {
    return [this.canvas.width, this.canvas.height];
  }

  /** Current scale relative to the fitted one. */
  fitRatio(): number {
    return this.view.scale / this.fitScale();
  }

  /** Springs to a view centred on the box (x, y, w, h), zoomed so it spans about a third of the canvas. */
  showRegion(x: number, y: number, w: number, h: number): void {
    if (!this.doc) return;
    const max = this.maxScale();
    const wanted = Math.min(this.canvas.width / (3 * w), this.canvas.height / (6 * h));
    const scale = Math.min(max, Math.max(this.fitScale(), wanted));
    const [cx, cy] = this.centre();
    this.go({ scale, x: x + w / 2 - cx / scale, y: y + h / 2 - cy / scale }, true);
  }

  /** Moves the drawing point (x, y) to the centre of the canvas, keeping the scale. */
  centreOn(x: number, y: number, animate: boolean): void {
    const { scale } = animate ? this.target() : this.view;
    const [cx, cy] = this.centre();
    const view = { scale, x: x - cx / scale, y: y - cy / scale };
    this.go(view, animate);
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
      const motion = this.motion;
      // A motion may hand over to another (momentum to a bounce), which then keeps running.
      if (!motion(dt) && this.motion === motion) this.motion = null;
    }
    const { width, height } = this.canvas;
    if (this.docDirty) {
      this.docDirty = false;
      const started = performance.now();
      if (this.doc && (this.doc.kind === 'vector' || (width > 0 && height > 0))) {
        drawDocument(this.ctx, this.doc, this.view, width, height, this.paper, this.weights);
        if (this.grid && this.doc.kind === 'vector') drawGrid(this.ctx, this.view, this.doc.origin, this.paper.invert);
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

  /** A trackpad pans with two fingers and zooms with a pinch; a mouse wheel zooms. */
  private onWheel(e: WheelEvent): void {
    e.preventDefault();
    const line = e.deltaMode === WheelEvent.DOM_DELTA_LINE;
    // Firefox scrolls a mouse wheel by lines. Chrome and Safari report a trackpad's legacy wheelDelta as
    // exactly -3 × its deltaY, and a mouse notch differently; mice rarely scroll sideways.
    const legacy = (e as WheelEvent & { wheelDeltaY?: number }).wheelDeltaY;
    const trackpad = !line && (e.deltaX !== 0 || legacy === undefined || Math.abs(legacy + 3 * e.deltaY) <= 3);
    if (trackpad && !e.ctrlKey) {
      const dpr = window.devicePixelRatio || 1;
      const { scale, x, y } = this.view;
      return this.go({ scale, x: x + (e.deltaX * dpr) / scale, y: y + (e.deltaY * dpr) / scale }, false);
    }
    const pixels = line ? e.deltaY * 16 : e.deltaY;
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
    const slop = e.pointerType === 'touch' ? TOUCH_TAP_SLOP : TAP_SLOP;
    this.press = { x: e.clientX, y: e.clientY, slop, moved: false };
    // Caught mid-bounce, the view is held from wherever the rubber band has it.
    this.held = this.band(this.view, true);
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
    if (this.press && Math.hypot(e.clientX - this.press.x, e.clientY - this.press.y) > this.press.slop) this.press.moved = true;
    // Until it is clearly a drag, the drawing stays put so a tap lands where it was aimed.
    if (this.press && !this.press.moved) return;
    const dpr = window.devicePixelRatio || 1;
    const { scale, x, y } = this.held!;
    const last = this.drag[this.drag.length - 1];
    const dx = ((e.clientX - last.x) * dpr) / scale;
    const dy = ((e.clientY - last.y) * dpr) / scale;
    this.drag.push({ x: e.clientX, y: e.clientY, t: e.timeStamp });
    this.drag = this.drag.filter((p) => e.timeStamp - p.t <= VELOCITY_WINDOW);
    this.held = { scale, x: x - dx, y: y - dy };
    this.setView(this.band(this.held));
  }

  /** The drawing follows both fingers: it pans with their midpoint and scales with their distance. */
  private onPinch(): void {
    const before = this.pinch!;
    const now = this.pinchState();
    this.pinch = now;
    const dpr = window.devicePixelRatio || 1;
    const rect = this.canvas.getBoundingClientRect();
    const { scale, x, y } = this.held!;
    const panned = { scale, x: x - ((now.x - before.x) * dpr) / scale, y: y - ((now.y - before.y) * dpr) / scale };
    const next = scale * (now.distance / Math.max(1, before.distance));
    this.held = zoomedAt(panned, next, (now.x - rect.left) * dpr, (now.y - rect.top) * dpr);
    this.setView(this.band(this.held));
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

  /**
   * On release, a view pulled past the limits springs back; otherwise the drawing keeps the finger's
   * velocity and decelerates like a scroll view, bouncing if it reaches the limits.
   */
  private endDrag(e?: PointerEvent): void {
    const samples = this.drag?.filter((p) => !e || e.timeStamp - p.t <= VELOCITY_WINDOW);
    const tap = e && this.press && !this.press.moved;
    this.drag = this.press = this.held = null;
    this.canvas.classList.remove('dragging');
    const out = !this.inBounds(this.view);
    if (out) this.settle();
    if (tap) return this.onTap?.(e);
    if (out || !e || !samples || samples.length < 2 || reducedMotion.matches) return;
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
      if (!this.inBounds(this.view)) {
        this.settle(vx, vy);
        return false;
      }
      const decay = DECELERATION ** (dt * 1000);
      vx *= decay;
      vy *= decay;
      return Math.hypot(vx, vy) >= MIN_FLICK / 4;
    });
  }
}
