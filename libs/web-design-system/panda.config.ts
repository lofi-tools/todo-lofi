import { defineConfig } from '@pandacss/dev'
import pandaPreset from '@pandacss/preset-panda'
import { preset } from './src/preset'

export default defineConfig({
  // The Panda base preset supplies the default spacing scale, breakpoint
  // conditions, and utilities; our preset is listed after it so our tokens and
  // recipes take precedence.
  presets: [pandaPreset, preset],
  // Explicit preflight: the DS ships its own reset in src/styles.
  preflight: false,
  include: ['./src/**/*.{ts,tsx,astro}'],
  exclude: [],
  outdir: 'styled-system',
  jsxFramework: 'solid',
  // Class-generation settings. The showcase config MUST keep these identical
  // or DS components render unstyled (spec §4.2).
  prefix: undefined,
  // Readable, stable class and CSS-variable names: the global styles in
  // src/styles reference variables like `--wds-colors-canvas` directly.
  hash: false,
  separator: '_',
  minify: false,
})
