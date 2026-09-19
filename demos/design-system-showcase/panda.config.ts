import { defineConfig } from '@pandacss/dev'
import pandaPreset from '@pandacss/preset-panda'
import { preset } from 'web-design-system/preset'

/**
 * Extends the design system's preset so tokens, recipes, and patterns come
 * from one source of truth (decision #7).
 *
 * Every class-generation setting below MUST stay identical to
 * `libs/web-design-system/panda.config.ts` or the design system's components
 * render unstyled here (spec §4.2). `include` pulls the design system's source
 * into this project's static analysis so the CSS it needs is emitted.
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
  // Must match the design system config exactly (spec §4.2).
  hash: false,
  separator: '_',
  minify: false,
})
