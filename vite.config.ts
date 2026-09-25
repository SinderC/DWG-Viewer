import { readFileSync } from 'node:fs';
import { defineConfig, type Plugin } from 'vite';
import { viteSingleFile } from 'vite-plugin-singlefile';

// `import bytes from './x.wasm?base64'` yields the file as a base64 string, so the
// WASM ends up inside the single HTML file and is never fetched.
function base64Import(): Plugin {
  return {
    name: 'base64-import',
    load(id) {
      if (!id.endsWith('?base64')) return;
      const file = id.slice(0, -'?base64'.length);
      this.addWatchFile(file);
      return `export default ${JSON.stringify(readFileSync(file).toString('base64'))};`;
    },
  };
}

// Blocks all network access from the built page. Build only: the dev server needs its own client script.
const CSP = [
  "default-src 'none'",
  "script-src 'unsafe-inline' 'wasm-unsafe-eval'",
  "style-src 'unsafe-inline'",
  'img-src data: blob:',
].join('; ');

function contentSecurityPolicy(): Plugin {
  return {
    name: 'csp',
    apply: 'build',
    transformIndexHtml: () => [{ tag: 'meta', attrs: { 'http-equiv': 'Content-Security-Policy', content: CSP }, injectTo: 'head-prepend' }],
  };
}

export default defineConfig({
  root: 'web',
  build: { outDir: '../dist', emptyOutDir: true, modulePreload: { polyfill: false } },
  plugins: [base64Import(), contentSecurityPolicy(), viteSingleFile()],
});
