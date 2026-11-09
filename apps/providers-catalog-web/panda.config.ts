import { defineConfig } from '@pandacss/dev'
import pandaPreset from '@pandacss/preset-panda'
import { preset } from 'web-design-system/preset'

/**
 * Keep the class-generation settings identical to the design system and the
 * other web packages. `include` must scan the design system's Astro and Solid
 * source because the catalog uses its components.
 */
export default defineConfig({
  presets: [pandaPreset, preset],
  preflight: false,
  include: [
    './src/**/*.{ts,tsx,astro}',
    '../../libs/web-design-system/src/**/*.{ts,tsx,astro}',
  ],
  exclude: [],
  outdir: 'styled-system',
  jsxFramework: 'solid',
  prefix: undefined,
  hash: false,
  separator: '_',
  minify: false,
})
