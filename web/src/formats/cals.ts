import type { FormatPlugin } from './types';
import { RasterDoc, rasterDocument } from './wasm';

export const cals: FormatPlugin = {
  name: 'CALS raster',
  extensions: ['cal', 'cals', 'ct1', 'c4', 'gp4', 'mil'],

  sniff(bytes) {
    const header = new TextDecoder('latin1').decode(bytes.subarray(0, 2048)).toLowerCase();
    return header.includes('rtype:') && header.includes('rpelcnt:');
  },

  open: (bytes) => rasterDocument(RasterDoc.openCals(bytes)),
};
