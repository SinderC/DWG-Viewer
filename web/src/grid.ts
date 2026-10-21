import type { View } from './formats/types';

/** Minor grid lines are a power of ten apart, the smallest at least this many CSS pixels apart. */
const MIN_SPACING_PX = 10;
/** Minor lines per major line, as in AutoCAD with a decimal grid. */
const MAJOR_EVERY = 10;
/** X axis red, Y axis green, as in AutoCAD. */
const X_AXIS = 'rgb(205 60 60)';
const Y_AXIS = 'rgb(70 170 70)';

/**
 * Draws an AutoCAD-style grid over the whole canvas, with the world axes through the origin.
 * Lines are screened onto dark paper and multiplied onto white, so they stay under the drawing.
 * `origin` is the world position of drawing (0, 0); world Y points up, drawing Y down.
 */
export function drawGrid(ctx: CanvasRenderingContext2D, view: View, origin: [number, number], dark: boolean): void {
  const { scale, x, y } = view;
  const { width, height } = ctx.canvas;
  const step = 10 ** Math.ceil(Math.log10((MIN_SPACING_PX * (window.devicePixelRatio || 1)) / scale));
  // Device position of world coordinate `w` along each axis; lines sit on pixel centres.
  const px = (w: number) => Math.round((w - origin[0] - x) * scale) + 0.5;
  const py = (w: number) => Math.round((origin[1] - w - y) * scale) + 0.5;
  const ink = dark ? '255 255 255' : '0 0 0';

  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalCompositeOperation = dark ? 'screen' : 'multiply';
  ctx.lineWidth = 1;
  const lines = (major: boolean) => {
    ctx.beginPath();
    const first = Math.floor((origin[0] + x) / step);
    for (let i = first; i <= first + width / (step * scale) + 1; i++) {
      if ((i % MAJOR_EVERY === 0) !== major) continue;
      ctx.moveTo(px(i * step), 0);
      ctx.lineTo(px(i * step), height);
    }
    const top = Math.ceil((origin[1] - y) / step);
    for (let i = top; i >= top - height / (step * scale) - 1; i--) {
      if ((i % MAJOR_EVERY === 0) !== major) continue;
      ctx.moveTo(0, py(i * step));
      ctx.lineTo(width, py(i * step));
    }
    ctx.strokeStyle = `rgb(${ink} / ${major ? 0.14 : 0.06})`;
    ctx.stroke();
  };
  lines(false);
  lines(true);

  ctx.beginPath();
  ctx.moveTo(0, py(0));
  ctx.lineTo(width, py(0));
  ctx.strokeStyle = X_AXIS;
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(px(0), 0);
  ctx.lineTo(px(0), height);
  ctx.strokeStyle = Y_AXIS;
  ctx.stroke();
  ctx.globalCompositeOperation = 'source-over';
}
