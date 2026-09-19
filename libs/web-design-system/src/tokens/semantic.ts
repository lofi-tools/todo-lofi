/**
 * Semantic color tokens.
 *
 * Dark is the base (the app is dark-first: an inline script sets
 * `data-theme="dark"` before paint), and `_light` overrides every role when
 * the toggle flips to light. Recipes read only these roles.
 */
export const semanticColors = {
  canvas: {
    value: { base: '{colors.gray.975}', _light: '{colors.white}' },
  },
  surface: {
    value: { base: '{colors.gray.950}', _light: '{colors.white}' },
  },
  'surface.subtle': {
    value: { base: '{colors.gray.925}', _light: '{colors.gray.50}' },
  },
  'surface.elevated': {
    value: { base: '{colors.gray.900}', _light: '{colors.white}' },
  },
  'surface.hover': {
    value: { base: '{colors.gray.850}', _light: '{colors.gray.100}' },
  },
  'surface.active': {
    value: { base: '{colors.gray.800}', _light: '{colors.gray.200}' },
  },
  overlay: {
    value: { base: 'rgba(0, 0, 0, 0.6)', _light: 'rgba(13, 14, 15, 0.4)' },
  },

  'fg.default': {
    value: { base: '{colors.gray.50}', _light: '{colors.gray.950}' },
  },
  // muted/subtle are both kept above 4.5:1 on the canvas in each theme
  // (spec §9); subtle is the weaker of the two in both directions.
  'fg.muted': {
    value: { base: '{colors.gray.400}', _light: '{colors.gray.700}' },
  },
  'fg.subtle': {
    value: { base: '{colors.gray.500}', _light: '{colors.gray.600}' },
  },
  'fg.onAccent': {
    value: { base: '{colors.white}', _light: '{colors.white}' },
  },

  border: {
    value: { base: '{colors.gray.850}', _light: '{colors.gray.200}' },
  },
  'border.strong': {
    value: { base: '{colors.gray.800}', _light: '{colors.gray.300}' },
  },
  'border.subtle': {
    value: { base: 'rgba(255, 255, 255, 0.06)', _light: 'rgba(13, 14, 15, 0.06)' },
  },

  accent: {
    value: { base: '{colors.brand.500}', _light: '{colors.brand.500}' },
  },
  'accent.hover': {
    value: { base: '{colors.brand.400}', _light: '{colors.brand.600}' },
  },
  'accent.subtle': {
    value: { base: 'rgba(94, 106, 210, 0.16)', _light: 'rgba(94, 106, 210, 0.1)' },
  },
  // `accent` is a fill (white text sits on it); accent-tinted *text* needs a
  // lighter tint in dark mode and a darker one in light mode to clear AA.
  'accent.text': {
    value: { base: '{colors.brand.300}', _light: '{colors.brand.600}' },
  },

  danger: {
    value: { base: '{colors.red.500}', _light: '{colors.red.500}' },
  },
  // A red dark enough for white text to clear 4.5:1 (red.500 is too light).
  'danger.fill': {
    value: { base: '{colors.red.600}', _light: '{colors.red.600}' },
  },
  'danger.subtle': {
    value: { base: 'rgba(235, 87, 87, 0.16)', _light: 'rgba(235, 87, 87, 0.1)' },
  },
  success: {
    value: { base: '{colors.green.500}', _light: '{colors.green.500}' },
  },
  'success.subtle': {
    value: { base: 'rgba(76, 183, 130, 0.16)', _light: 'rgba(76, 183, 130, 0.12)' },
  },
  warning: {
    value: { base: '{colors.yellow.500}', _light: '{colors.yellow.500}' },
  },
  'warning.subtle': {
    value: { base: 'rgba(242, 201, 76, 0.16)', _light: 'rgba(242, 201, 76, 0.12)' },
  },
  info: {
    value: { base: '{colors.blue.400}', _light: '{colors.blue.500}' },
  },

  // Foregrounds for badges and label chips: readable text on the tinted
  // `*.subtle` backgrounds, which need a lighter tint in dark mode and a
  // darker one in light mode to clear 4.5:1 at small sizes.
  'badge.accent': {
    value: { base: '{colors.brand.300}', _light: '{colors.brand.600}' },
  },
  'badge.success': {
    value: { base: '{colors.green.400}', _light: '{colors.green.600}' },
  },
  'badge.warning': {
    value: { base: '{colors.yellow.400}', _light: '{colors.yellow.600}' },
  },
  'badge.danger': {
    value: { base: '{colors.red.400}', _light: '{colors.red.600}' },
  },
  'badge.info': {
    value: { base: '{colors.blue.400}', _light: '{colors.blue.600}' },
  },
} as const

/** Fixed label colors, keyed to the label names used across the mock UI. */
export const labelColors = {
  bug: { value: { base: '#eb5757', _light: '#c74444' } },
  feature: { value: { base: '#5e6ad2', _light: '#4f5ac2' } },
  improvement: { value: { base: '#4cb782', _light: '#3d9a6a' } },
  design: { value: { base: '#8b5cf6', _light: '#7c4ce0' } },
  performance: { value: { base: '#f2994a', _light: '#d97f31' } },
  ai: { value: { base: '#60a5fa', _light: '#3b82f6' } },
} as const
