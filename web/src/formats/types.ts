/** Visible region: `scale` device pixels per drawing unit, `(x, y)` = drawing coordinate at the canvas' top-left. */
export interface View {
  scale: number;
  x: number;
  y: number;
}

export interface RasterDocument {
  kind: 'raster';
  width: number;
  height: number;
  dpi: number;
  pageCount: number;
  /** Format-specific properties (compression, header fields, …) as label/value rows. */
  info: [string, string][];
  /** `invert` swaps black and white, for files encoded with the wrong polarity. */
  render(view: View, width: number, height: number, invert: boolean): ImageData;
  free(): void;
}

export interface Layer {
  name: string;
  /** CSS colour on white paper. */
  color: string;
  /** Set to show or hide the layer; takes effect on the next `draw()`. */
  visible: boolean;
}

export interface VectorDocument {
  kind: 'vector';
  /** Extents in drawing units. */
  width: number;
  height: number;
  /** Length unit ("mm", "in", …), or "" if the file does not say. */
  units: string;
  /** World coordinates of drawing (0, 0); world Y points up, drawing Y down. */
  origin: [number, number];
  pageCount: number;
  /** Format-specific properties (version, layers, …) as label/value rows. */
  info: [string, string][];
  /** In name order; initially visible unless off or frozen in the file. */
  layers: Layer[];
  /** Draws the whole canvas. `invert` puts the drawing on black instead of white paper. */
  draw(ctx: CanvasRenderingContext2D, view: View, invert: boolean): void;
  free(): void;
}

export type DrawingDocument = RasterDocument | VectorDocument;

export interface FormatPlugin {
  name: string;
  extensions: string[];
  /** True if the bytes look like this format. */
  sniff(bytes: Uint8Array): boolean;
  /** Opens page `page` (0-based) of a multi-page file. */
  open(bytes: Uint8Array, page?: number): DrawingDocument;
}
