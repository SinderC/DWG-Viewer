import { vectorDocument } from './dxf';
import type { FormatPlugin } from './types';
import { DxfDoc } from './wasm';

export const cgm: FormatPlugin = {
  name: 'CGM',
  extensions: ['cgm'],

  // A binary metafile starts with BEGIN METAFILE: class 0, element 1; a clear-text one with BEGMF.
  sniff: (bytes) =>
    (bytes.length >= 2 && bytes[0] === 0 && (bytes[1] & 0xe0) === 0x20) ||
    /^\s*BEG_?MF/i.test(new TextDecoder('latin1').decode(bytes.subarray(0, 64))),

  open: (bytes, page = 0) => vectorDocument(DxfDoc.openCgm(bytes, page)),
};
