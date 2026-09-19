/**
 * Spacing scale (0.25rem base, matching Panda's convention).
 *
 * Defined explicitly rather than inherited: the shared preset is the only
 * preset both packages generate from, so the scale must travel with it.
 */
export const spacing = {
  0: { value: '0' },
  0.5: { value: '0.125rem' },
  1: { value: '0.25rem' },
  1.5: { value: '0.375rem' },
  2: { value: '0.5rem' },
  2.5: { value: '0.625rem' },
  3: { value: '0.75rem' },
  3.5: { value: '0.875rem' },
  4: { value: '1rem' },
  4.5: { value: '1.125rem' },
  5: { value: '1.25rem' },
  6: { value: '1.5rem' },
  7: { value: '1.75rem' },
  8: { value: '2rem' },
  9: { value: '2.25rem' },
  10: { value: '2.5rem' },
  11: { value: '2.75rem' },
  12: { value: '3rem' },
  14: { value: '3.5rem' },
  16: { value: '4rem' },
  20: { value: '5rem' },
  24: { value: '6rem' },
  28: { value: '7rem' },
  32: { value: '8rem' },
  40: { value: '10rem' },
  48: { value: '12rem' },
  56: { value: '14rem' },
  64: { value: '16rem' },
} as const

/** `sizes` mirrors spacing so width/height utilities resolve to the same scale. */
export const sizes = {
  ...spacing,
  xs: { value: '20rem' },
  sm: { value: '24rem' },
  md: { value: '28rem' },
  lg: { value: '32rem' },
  xl: { value: '36rem' },
  full: { value: '100%' },
  min: { value: 'min-content' },
  max: { value: 'max-content' },
  fit: { value: 'fit-content' },
} as const
