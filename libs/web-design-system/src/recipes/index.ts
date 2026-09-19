/**
 * Panda recipes for the styled layer.
 *
 * These live in the preset (not in a component file) so the showcase's
 * codegen produces byte-identical class names from the same definitions —
 * the class-parity requirement in spec §4.2. Components call the generated
 * recipe functions from `styled-system/recipes`.
 */

export const button = {
  className: 'button',
  base: {
    display: 'inline-flex',
    alignItems: 'center',
    justifyContent: 'center',
    gap: '2',
    fontFamily: 'sans',
    fontWeight: 'medium',
    lineHeight: 'none',
    whiteSpace: 'nowrap',
    borderRadius: 'sm',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    cursor: 'pointer',
    transitionProperty: 'background-color, border-color, color, box-shadow, opacity',
    transitionDuration: 'fast',
    transitionTimingFunction: 'standard',
    userSelect: 'none',
    textDecoration: 'none',
    _focusVisible: { outline: 'none', boxShadow: 'focus' },
    _disabled: { opacity: '0.5', cursor: 'not-allowed', pointerEvents: 'none' },
    '& svg': { flexShrink: '0' },
  },
  variants: {
    variant: {
      primary: {
        bg: 'accent',
        color: 'fg.onAccent',
        borderColor: 'accent',
        _hover: { bg: 'accent.hover', borderColor: 'accent.hover' },
      },
      secondary: {
        bg: 'surface.elevated',
        color: 'fg.default',
        borderColor: 'border',
        _hover: { bg: 'surface.hover', borderColor: 'border.strong' },
      },
      ghost: {
        bg: 'transparent',
        color: 'fg.muted',
        borderColor: 'transparent',
        _hover: { bg: 'surface.hover', color: 'fg.default' },
      },
      danger: {
        bg: 'danger.fill',
        color: 'fg.onAccent',
        borderColor: 'danger.fill',
        _hover: { opacity: '0.9' },
      },
      subtle: {
        bg: 'accent.subtle',
        color: 'accent.text',
        borderColor: 'transparent',
        _hover: { color: 'accent.hover' },
      },
    },
    size: {
      xs: { h: '6', px: '2', fontSize: 'xs' },
      sm: { h: '8', px: '3', fontSize: 'sm' },
      md: { h: '9', px: '4', fontSize: 'md' },
      lg: { h: '11', px: '5', fontSize: 'lg' },
    },
    fullWidth: { true: { width: '100%' } },
  },
  defaultVariants: { variant: 'secondary', size: 'md' },
}

export const iconButton = {
  className: 'iconButton',
  base: {
    display: 'inline-flex',
    alignItems: 'center',
    justifyContent: 'center',
    borderRadius: 'sm',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    borderColor: 'transparent',
    bg: 'transparent',
    color: 'fg.muted',
    cursor: 'pointer',
    transitionProperty: 'background-color, color, border-color, box-shadow',
    transitionDuration: 'fast',
    transitionTimingFunction: 'standard',
    _focusVisible: { outline: 'none', boxShadow: 'focus' },
    _hover: { bg: 'surface.hover', color: 'fg.default' },
    _disabled: { opacity: '0.5', cursor: 'not-allowed', pointerEvents: 'none' },
  },
  variants: {
    variant: {
      ghost: {},
      outline: { borderColor: 'border', bg: 'surface.elevated' },
      solid: { bg: 'accent', color: 'fg.onAccent', _hover: { bg: 'accent.hover' } },
    },
    size: {
      xs: { h: '6', w: '6', fontSize: 'sm' },
      sm: { h: '8', w: '8', fontSize: 'md' },
      md: { h: '9', w: '9', fontSize: 'lg' },
      lg: { h: '11', w: '11', fontSize: 'xl' },
    },
  },
  defaultVariants: { variant: 'ghost', size: 'sm' },
}

export const badge = {
  className: 'badge',
  base: {
    display: 'inline-flex',
    alignItems: 'center',
    gap: '1.5',
    h: '5',
    px: '2',
    borderRadius: 'full',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    borderColor: 'transparent',
    fontFamily: 'sans',
    fontSize: '2xs',
    fontWeight: 'medium',
    lineHeight: 'none',
    whiteSpace: 'nowrap',
  },
  variants: {
    tone: {
      neutral: { bg: 'surface.hover', color: 'fg.muted', borderColor: 'border' },
      accent: { bg: 'accent.subtle', color: 'badge.accent' },
      success: { bg: 'success.subtle', color: 'badge.success' },
      warning: { bg: 'warning.subtle', color: 'badge.warning' },
      danger: { bg: 'danger.subtle', color: 'badge.danger' },
      info: { bg: 'accent.subtle', color: 'badge.info' },
    },
    variant: {
      subtle: {},
      solid: {
        bg: 'fg.default',
        color: 'canvas',
        borderColor: 'fg.default',
      },
      outline: { bg: 'transparent', color: 'fg.muted', borderColor: 'border' },
    },
  },
  defaultVariants: { tone: 'neutral', variant: 'subtle' },
}

export const input = {
  className: 'input',
  base: {
    display: 'flex',
    alignItems: 'center',
    width: '100%',
    h: '9',
    px: '3',
    borderRadius: 'sm',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    borderColor: 'border',
    bg: 'surface.subtle',
    color: 'fg.default',
    fontFamily: 'sans',
    fontSize: 'md',
    lineHeight: 'normal',
    transitionProperty: 'border-color, box-shadow, background-color',
    transitionDuration: 'fast',
    transitionTimingFunction: 'standard',
    _placeholder: { color: 'fg.subtle' },
    _hover: { borderColor: 'border.strong' },
    _focusVisible: { outline: 'none', borderColor: 'accent', boxShadow: 'focus' },
    _disabled: { opacity: '0.5', cursor: 'not-allowed' },
  },
  variants: {
    size: {
      sm: { h: '8', fontSize: 'sm' },
      md: { h: '9', fontSize: 'md' },
      lg: { h: '11', fontSize: 'lg' },
    },
  },
  defaultVariants: { size: 'md' },
}

export const card = {
  className: 'card',
  base: {
    bg: 'surface.subtle',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    borderColor: 'border',
    borderRadius: 'lg',
    color: 'fg.default',
    overflow: 'hidden',
  },
  variants: {
    padding: {
      none: { p: '0' },
      sm: { p: '3' },
      md: { p: '6' },
      lg: { p: '8' },
    },
    interactive: {
      true: {
        transitionProperty: 'background-color, border-color, box-shadow',
        transitionDuration: 'fast',
        transitionTimingFunction: 'standard',
        _hover: { bg: 'surface.elevated', borderColor: 'border.strong' },
      },
    },
  },
  defaultVariants: { padding: 'md' },
}

export const avatar = {
  className: 'avatar',
  base: {
    display: 'inline-grid',
    placeItems: 'center',
    borderRadius: 'full',
    overflow: 'hidden',
    bg: 'surface.elevated',
    color: 'fg.muted',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    borderColor: 'border',
    fontFamily: 'sans',
    fontWeight: 'medium',
    lineHeight: 'none',
    flexShrink: '0',
    userSelect: 'none',
  },
  variants: {
    size: {
      xs: { h: '5', w: '5', fontSize: '2xs' },
      sm: { h: '6', w: '6', fontSize: 'xs' },
      md: { h: '8', w: '8', fontSize: 'sm' },
      lg: { h: '11', w: '11', fontSize: 'lg' },
      xl: { h: '16', w: '16', fontSize: '2xl' },
    },
  },
  defaultVariants: { size: 'md' },
}

export const kbd = {
  className: 'kbd',
  base: {
    display: 'inline-flex',
    alignItems: 'center',
    justifyContent: 'center',
    minW: '5',
    h: '5',
    px: '1.5',
    borderRadius: 'xs',
    borderWidth: 'hairline',
    borderStyle: 'solid',
    borderColor: 'border',
    bg: 'surface.elevated',
    color: 'fg.muted',
    fontFamily: 'mono',
    fontSize: '2xs',
    lineHeight: 'none',
    boxShadow: 'xs',
  },
}

export const statusDot = {
  className: 'statusDot',
  base: {
    display: 'inline-block',
    w: '2',
    h: '2',
    borderRadius: 'full',
    flexShrink: '0',
  },
  variants: {
    tone: {
      neutral: { bg: 'fg.subtle' },
      accent: { bg: 'accent' },
      success: { bg: 'success' },
      warning: { bg: 'warning' },
      danger: { bg: 'danger' },
      info: { bg: 'info' },
    },
  },
  defaultVariants: { tone: 'neutral' },
}
