/**
 * Raw color palette.
 *
 * Values are the ones extracted from linear.app's computed styles (see
 * `src/tokens/EXTRACTION.md`) and normalized by hand into a usable scale.
 * Components must never consume these directly — they use the semantic tokens
 * in `semantic.ts`, so light mode is a token swap rather than a component
 * change.
 */
export const colors = {
  white: { value: '#ffffff' },
  black: { value: '#000000' },

  gray: {
    25: { value: '#fcfcfd' },
    50: { value: '#f8f8f8' },
    100: { value: '#f3f4f5' },
    200: { value: '#e8e9eb' },
    300: { value: '#d9dbde' },
    400: { value: '#b4b8bf' },
    500: { value: '#8a8f98' },
    600: { value: '#62666d' },
    700: { value: '#45484d' },
    800: { value: '#2a2c30' },
    850: { value: '#1f2023' },
    900: { value: '#191a1c' },
    925: { value: '#101113' },
    950: { value: '#0d0e0f' },
    975: { value: '#08090a' },
  },

  /** Linear's signature indigo. */
  brand: {
    300: { value: '#a5adff' },
    400: { value: '#828fff' },
    500: { value: '#5e6ad2' },
    600: { value: '#4f5ac2' },
    700: { value: '#3f4aa0' },
  },

  violet: {
    400: { value: '#a78bfa' },
    500: { value: '#8b5cf6' },
  },
  blue: {
    400: { value: '#60a5fa' },
    500: { value: '#3b82f6' },
    600: { value: '#2563eb' },
  },
  green: {
    400: { value: '#5fca8b' },
    500: { value: '#4cb782' },
    600: { value: '#2f7d5b' },
  },
  yellow: {
    400: { value: '#f6cf5c' },
    500: { value: '#f2c94c' },
    600: { value: '#8a6a00' },
  },
  orange: {
    400: { value: '#f2994a' },
    500: { value: '#eb8c3f' },
  },
  red: {
    400: { value: '#f07171' },
    500: { value: '#eb5757' },
    600: { value: '#c74444' },
  },
} as const
