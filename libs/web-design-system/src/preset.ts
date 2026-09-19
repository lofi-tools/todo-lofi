import {
  colors,
  spacing,
  sizes,
  semanticColors,
  labelColors,
  fonts,
  fontSizes,
  fontWeights,
  lineHeights,
  letterSpacings,
  radii,
  shadows,
  blurs,
  borderWidths,
  breakpoints,
  durations,
  easings,
  zIndex,
} from './tokens'
import {
  button,
  iconButton,
  badge,
  input,
  card,
  avatar,
  kbd,
  statusDot,
} from './recipes'

/**
 * The shared Panda preset (decision #7).
 *
 * Consumers extend it in their own `panda.config.ts`; because both sides
 * generate from these exact definitions, atomic class names line up across
 * the package boundary (spec §4.2).
 *
 * Plain objects rather than `definePreset` so importing this preset never
 * pulls `@pandacss/dev` into a runtime bundle.
 */
export const preset = {
  name: 'web-design-system',
  conditions: {
    extend: {
      // Base values are the dark theme; `_light` restores the light palette.
      light: '[data-theme="light"] &',
    },
  },
  theme: {
    tokens: {
      colors,
      spacing,
      sizes,
      fonts,
      fontSizes,
      fontWeights,
      lineHeights,
      letterSpacings,
      radii,
      shadows,
      blurs,
      borderWidths,
      breakpoints,
      durations,
      easings,
      zIndex,
    },
    semanticTokens: {
      colors: {
        ...semanticColors,
        ...labelColors,
      },
    },
    keyframes: {
      spin: {
        from: { transform: 'rotate(0deg)' },
        to: { transform: 'rotate(360deg)' },
      },
      pulse: {
        '0%, 100%': { opacity: '0.5' },
        '50%': { opacity: '1' },
      },
      fadeIn: {
        from: { opacity: '0' },
        to: { opacity: '1' },
      },
      slideUp: {
        from: { opacity: '0', transform: 'translateY(6px)' },
        to: { opacity: '1', transform: 'translateY(0)' },
      },
    },
    textStyles: {
      display: {
        value: {
          fontSize: '7xl',
          lineHeight: 'tight',
          letterSpacing: 'tighter',
          fontWeight: 'semibold',
        },
      },
      'display.lg': {
        value: {
          fontSize: '8xl',
          lineHeight: 'none',
          letterSpacing: 'tighter',
          fontWeight: 'semibold',
        },
      },
      h1: {
        value: {
          fontSize: '5xl',
          lineHeight: 'tight',
          letterSpacing: 'tighter',
          fontWeight: 'semibold',
        },
      },
      h2: {
        value: {
          fontSize: '4xl',
          lineHeight: 'snug',
          letterSpacing: 'tight',
          fontWeight: 'semibold',
        },
      },
      h3: {
        value: {
          fontSize: '3xl',
          lineHeight: 'snug',
          letterSpacing: 'tight',
          fontWeight: 'semibold',
        },
      },
      h4: {
        value: {
          fontSize: '2xl',
          lineHeight: 'snug',
          letterSpacing: 'normal',
          fontWeight: 'semibold',
        },
      },
      body: {
        value: {
          fontSize: 'md',
          lineHeight: 'relaxed',
          letterSpacing: 'normal',
          fontWeight: 'normal',
        },
      },
      'body.lg': {
        value: {
          fontSize: 'lg',
          lineHeight: 'relaxed',
          letterSpacing: 'normal',
          fontWeight: 'normal',
        },
      },
      caption: {
        value: {
          fontSize: 'sm',
          lineHeight: 'normal',
          letterSpacing: 'normal',
          fontWeight: 'normal',
        },
      },
      micro: {
        value: {
          fontSize: '2xs',
          lineHeight: 'normal',
          letterSpacing: 'wide',
          fontWeight: 'medium',
        },
      },
      code: {
        value: {
          fontSize: 'sm',
          lineHeight: 'normal',
          letterSpacing: 'normal',
          fontWeight: 'normal',
          fontFamily: 'mono',
        },
      },
    },
    recipes: {
      button,
      iconButton,
      badge,
      input,
      card,
      avatar,
      kbd,
      statusDot,
    },
  },
} as const

export default preset
