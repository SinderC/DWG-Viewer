import { vectorDocument } from './dxf';
import type { FormatPlugin } from './types';
import { DxfDoc } from './wasm';

/** Classic DWF streams and DWF 6 packages start with "(DWF V"; DWFx is a plain ZIP, found by extension. */
const DWF = /^\(DWF V\d\d\.\d\d\)/;

export const dwf: FormatPlugin = {
  name: 'DWF',
  extensions: ['dwf', 'dwfx'],

  sniff: (bytes) => DWF.test(new TextDecoder('latin1').decode(bytes.subarray(0, 12))),

  open: (bytes, page = 0) => vectorDocument(DxfDoc.openDwf(bytes, page)),
};
