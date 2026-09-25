import type { FormatPlugin } from './types';
import { RasterDoc, rasterDocument, tiffPageCount } from './wasm';

/** Byte order mark plus version: 42 for classic TIFF, 43 for BigTIFF. */
const MAGIC = ['II*\0', 'MM\0*', 'II+\0', 'MM\0+'];

export const tiff: FormatPlugin = {
  name: 'TIFF',
  extensions: ['tif', 'tiff'],

  sniff: (bytes) => MAGIC.includes(String.fromCharCode(...bytes.subarray(0, 4))),

  open: (bytes, page = 0) => rasterDocument(RasterDoc.openTiff(bytes, page), tiffPageCount(bytes)),
};
