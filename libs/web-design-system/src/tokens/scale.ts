/** Radii, shadows, blur, borders, breakpoints, and motion. */

export const radii = {
  none: { value: '0' },
  xs: { value: '3px' },
  sm: { value: '5px' },
  md: { value: '8px' },
  lg: { value: '12px' },
  xl: { value: '16px' },
  '2xl': { value: '24px' },
  full: { value: '9999px' },
} as const

/**
 * Layered, low-opacity shadows rather than one heavy drop shadow, so
 * elevation reads on both the near-black canvas and the light theme.
 */
export const shadows = {
  xs: { value: '0 1px 2px 0 rgba(0, 0, 0, 0.2)' },
  sm: {
    value:
      '0 1px 2px 0 rgba(0, 0, 0, 0.25), 0 0 0 1px rgba(255, 255, 255, 0.02)',
  },
  md: {
    value:
      '0 4px 12px -2px rgba(0, 0, 0, 0.4), 0 0 0 1px rgba(255, 255, 255, 0.03)',
  },
  lg: {
    value:
      '0 12px 32px -6px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.04)',
  },
  xl: {
    value:
      '0 24px 64px -12px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.05)',
  },
  focus: { value: '0 0 0 2px rgba(94, 106, 210, 0.55)' },
} as const

export const blurs = {
  sm: { value: '4px' },
  md: { value: '12px' },
  lg: { value: '24px' },
} as const

export const borderWidths = {
  0: { value: '0' },
  hairline: { value: '1px' },
  thick: { value: '2px' },
} as const

/** Linear-like content is a centered column, collapsing at these widths. */
export const breakpoints = {
  sm: { value: '640px' },
  md: { value: '768px' },
  lg: { value: '1024px' },
  xl: { value: '1280px' },
  '2xl': { value: '1536px' },
} as const

export const durations = {
  instant: { value: '0ms' },
  fast: { value: '120ms' },
  normal: { value: '200ms' },
  slow: { value: '320ms' },
  scene: { value: '600ms' },
} as const

export const easings = {
  standard: { value: 'cubic-bezier(0.2, 0, 0, 1)' },
  emphasized: { value: 'cubic-bezier(0.3, 0, 0, 1.2)' },
  linear: { value: 'linear' },
} as const

/** Named blur sizes for the sticky navbar's backdrop. */
export const zIndex = {
  base: { value: '0' },
  raised: { value: '10' },
  sticky: { value: '100' },
  overlay: { value: '200' },
  popover: { value: '300' },
  toast: { value: '400' },
} as const
