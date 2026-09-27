import { DxfDoc, dxfBackground, dxfForeground, initSync, RasterDoc, tiffPageCount } from '../../pkg/raster.js';
import wasmBase64 from '../../pkg/raster_bg.wasm?base64';
import type { RasterDocument, View } from './types';

const wasm = initSync({ module: Uint8Array.from(atob(wasmBase64), (c) => c.charCodeAt(0)) });

export { DxfDoc, dxfBackground, dxfForeground, RasterDoc, tiffPageCount };

/** Pairs up the label, value, label, value, … list returned by the WASM `info()` methods. */
export function infoRows(flat: string[]): [string, string][] {
  const rows: [string, string][] = [];
  for (let i = 0; i + 1 < flat.length; i += 2) rows.push([flat[i], flat[i + 1]]);
  return rows;
}

/** Renders a `width` × `height` viewport of `doc`, in image pixels. */
export function renderRaster(doc: RasterDoc, view: View, width: number, height: number, invert: boolean): ImageData {
  const ptr = doc.render(view.scale, view.x, view.y, invert, width, height);
  // Read the view after render(): the call may have grown (and replaced) WASM memory.
  const pixels = new Uint8ClampedArray(wasm.memory.buffer, ptr, width * height * 4);
  return new ImageData(pixels, width, height);
}

/** Adapts a WASM `RasterDoc` to the viewer's document interface. */
export function rasterDocument(doc: RasterDoc, pageCount = 1): RasterDocument {
  return {
    kind: 'raster',
    width: doc.width,
    height: doc.height,
    dpi: doc.dpi,
    pageCount,
    info: infoRows(doc.info()),
    render: (view, width, height, invert) => renderRaster(doc, view, width, height, invert),
    free: () => doc.free(),
  };
}
