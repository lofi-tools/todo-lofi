/** Typography tokens. Inter Variable is self-hosted (decision #20). */
export const fonts = {
  sans: {
    value:
      "'Inter Variable', 'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
  },
  mono: {
    value: "'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
  },
} as const

export const fontSizes = {
  '2xs': { value: '0.6875rem' }, // 11px
  xs: { value: '0.75rem' }, // 12px
  sm: { value: '0.8125rem' }, // 13px
  md: { value: '0.875rem' }, // 14px
  lg: { value: '0.9375rem' }, // 15px
  xl: { value: '1rem' }, // 16px
  '2xl': { value: '1.125rem' }, // 18px
  '3xl': { value: '1.25rem' }, // 20px
  '4xl': { value: '1.5rem' }, // 24px
  '5xl': { value: '2rem' }, // 32px
  '6xl': { value: '2.75rem' }, // 44px
  '7xl': { value: '3.5rem' }, // 56px
  '8xl': { value: '4.5rem' }, // 72px
} as const

export const fontWeights = {
  normal: { value: '400' },
  medium: { value: '500' },
  semibold: { value: '600' },
  bold: { value: '700' },
} as const

export const lineHeights = {
  none: { value: '1' },
  tight: { value: '1.15' },
  snug: { value: '1.3' },
  normal: { value: '1.5' },
  relaxed: { value: '1.7' },
} as const

export const letterSpacings = {
  tighter: { value: '-0.03em' },
  tight: { value: '-0.02em' },
  normal: { value: '0' },
  wide: { value: '0.01em' },
  wider: { value: '0.04em' },
} as const
