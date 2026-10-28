// Bundles the extension into dist/extension.cjs (vscode is provided by the host) and the webviews into
// dist/webview/<entry>.{js,css} (browser code, nothing loaded at runtime). Tests run the .ts sources directly.
import * as esbuild from 'esbuild';

const minify = process.env.NODE_ENV === 'production';

const extension = {
  entryPoints: ['src/extension.ts'],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node20',
  outfile: 'dist/extension.cjs',
  external: ['vscode'],
  sourcemap: true,
  minify,
  logLevel: 'info',
};

// Webviews run in VS Code's Chromium; localResourceRoots is dist/webview, so everything they load must land there.
const webview = {
  entryPoints: { table: 'webview/table.ts', er: 'webview/er.ts' },
  bundle: true,
  platform: 'browser',
  format: 'iife',
  target: 'chrome120',
  outdir: 'dist/webview',
  sourcemap: minify ? false : 'inline',
  minify,
  logLevel: 'info',
};

if (process.argv.includes('--watch')) {
  await Promise.all([extension, webview].map(async (o) => (await esbuild.context(o)).watch()));
} else {
  await Promise.all([esbuild.build(extension), esbuild.build(webview)]);
}
