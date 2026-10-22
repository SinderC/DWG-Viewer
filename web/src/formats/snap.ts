import type { SnapPoint } from './types';

const KINDS: SnapPoint['kind'][] = ['end', 'mid', 'centre'];

/**
 * Snap points of a drawing, with their layers. Filled while the geometry is read, up to `capacity`;
 * on the first query they are bucketed into a uniform grid of about one point per cell.
 */
export class SnapPoints {
  private n = 0;
  private xs: Float64Array;
  private ys: Float64Array;
  /** Index into KINDS. */
  private kinds: Uint8Array;
  private layers: Uint32Array;
  private grid?: { x0: number; y0: number; cell: number; cols: number; rows: number; start: Uint32Array; order: Uint32Array };

  constructor(capacity: number) {
    this.xs = new Float64Array(capacity);
    this.ys = new Float64Array(capacity);
    this.kinds = new Uint8Array(capacity);
    this.layers = new Uint32Array(capacity);
  }

  add(x: number, y: number, kind: SnapPoint['kind'], layer: number): void {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return;
    const p = this.n++;
    this.xs[p] = x;
    this.ys[p] = y;
    this.kinds[p] = KINDS.indexOf(kind);
    this.layers[p] = layer;
  }

  /** The nearest point within `radius` of (x, y) whose layer passes `visible`. */
  nearest(x: number, y: number, radius: number, visible: (layer: number) => boolean): SnapPoint | null {
    const { xs, ys } = this;
    const grid = (this.grid ??= this.build());
    const { x0, y0, cell, cols, rows, start, order } = grid;
    const clamp = (v: number, n: number) => Math.min(n - 1, Math.max(0, Math.floor(v)));
    const [c0, c1] = [clamp((x - radius - x0) / cell, cols), clamp((x + radius - x0) / cell, cols)];
    const [r0, r1] = [clamp((y - radius - y0) / cell, rows), clamp((y + radius - y0) / cell, rows)];
    let best = -1;
    let bestDistance = radius * radius;
    for (let r = r0; r <= r1; r++) {
      for (let c = c0; c <= c1; c++) {
        const i = r * cols + c;
        for (let k = start[i]; k < start[i + 1]; k++) {
          const p = order[k];
          const d = (xs[p] - x) ** 2 + (ys[p] - y) ** 2;
          if (d <= bestDistance && visible(this.layers[p])) [best, bestDistance] = [p, d];
        }
      }
    }
    return best < 0 ? null : { x: xs[best], y: ys[best], kind: KINDS[this.kinds[best]] };
  }

  /** Counting sort of the points by cell: `order[start[i]..start[i + 1]]` are the points in cell i. */
  private build() {
    const { xs, ys, n } = this;
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    for (let p = 0; p < n; p++) {
      [x0, x1] = [Math.min(x0, xs[p]), Math.max(x1, xs[p])];
      [y0, y1] = [Math.min(y0, ys[p]), Math.max(y1, ys[p])];
    }
    if (!n) [x0, y0, x1, y1] = [0, 0, 1, 1];
    const side = Math.max(1, Math.ceil(Math.sqrt(n)));
    const cell = Math.max(x1 - x0, y1 - y0, 1e-9) / side;
    const cols = Math.min(side, Math.floor((x1 - x0) / cell) + 1);
    const rows = Math.min(side, Math.floor((y1 - y0) / cell) + 1);
    const cellOf = (p: number) => Math.min(rows - 1, Math.floor((ys[p] - y0) / cell)) * cols + Math.min(cols - 1, Math.floor((xs[p] - x0) / cell));
    const start = new Uint32Array(cols * rows + 1);
    for (let p = 0; p < n; p++) start[cellOf(p) + 1]++;
    for (let i = 0; i < cols * rows; i++) start[i + 1] += start[i];
    const next = start.slice(0, -1);
    const order = new Uint32Array(n);
    for (let p = 0; p < n; p++) order[next[cellOf(p)]++] = p;
    return { x0, y0, cell, cols, rows, start, order };
  }
}
