import { vectorDocument } from './dxf';
import type { FormatPlugin } from './types';
import { DxfDoc } from './wasm';

export const cgm: FormatPlugin = {
  name: 'CGM',
  extensions: ['cgm'],

  // A binary metafile starts with BEGIN METAFILE: class 0, element 1.
  sniff: (bytes) => bytes.length >= 2 && bytes[0] === 0 && (bytes[1] & 0xe0) === 0x20,

  open: (bytes, page = 0) => vectorDocument(DxfDoc.openCgm(bytes, page)),
};
