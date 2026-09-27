import { vectorDocument } from './dxf';
import type { FormatPlugin } from './types';
import { DxfDoc } from './wasm';

/** PCL/PJL wrapper: Universal Exit Language, printer reset or "enter HP-GL/2". */
const PCL = /^[\s\0]*\x1b(%-12345X|E|%-?\d*B)/;
/** Plain HP-GL: an HP-GL/1 device-control escape, or a command files typically start with. */
const HPGL = /^[\s;\0]*(\x1b\.|(IN|DF|BP|PS|SP|PU|PD|PA|IP|SC|RO|NP|CO|PG)[\s\d;,.+\-"A-Z])/;

export const hpgl: FormatPlugin = {
  name: 'HP-GL',
  extensions: ['plt', 'hpgl', 'hpg', 'hgl', 'plo', 'rtl'],

  sniff(bytes) {
    const head = new TextDecoder('latin1').decode(bytes.subarray(0, 256));
    return PCL.test(head) || HPGL.test(head);
  },

  open: (bytes, page = 0) => vectorDocument(DxfDoc.openHpgl(bytes, page)),
};
