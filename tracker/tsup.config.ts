import { defineConfig } from 'tsup'

// A single self-initializing IIFE (dist/pt.js) that the Dullahan server vendors
// at server/assets/pt.js and serves at /pt.js. es2019 keeps old browsers happy.
// The package is no longer published, so there is no ESM/CJS/types build.
export default defineConfig({
  entry: { pt: 'src/auto.ts' },
  format: ['iife'],
  dts: false,
  clean: true,
  sourcemap: false,
  minify: true,
  treeshake: true,
  target: 'es2019',
  outExtension: () => ({ js: '.js' }),
})
