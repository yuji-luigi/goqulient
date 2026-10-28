// Bundles the extension into dist/extension.cjs (vscode is provided by the host). Tests run the .ts sources directly.
import * as esbuild from 'esbuild';

const options = {
  entryPoints: ['src/extension.ts'],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node20',
  outfile: 'dist/extension.cjs',
  external: ['vscode'],
  sourcemap: true,
  minify: process.env.NODE_ENV === 'production',
  logLevel: 'info',
};

if (process.argv.includes('--watch')) await (await esbuild.context(options)).watch();
else await esbuild.build(options);
