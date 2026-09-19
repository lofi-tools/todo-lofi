import { describe, expect, it } from 'vitest'
import { colors, semanticColors, labelColors, radii, durations } from '../src/tokens'

type Scale = Record<string, { value: string }>

const scales = colors as unknown as Record<string, Scale | { value: string }>

/** Resolve a `{colors.gray.500}` token reference to its literal value. */
function resolveRef(reference: string): string {
  const match = reference.match(/^\{colors\.([\w.]+)\}$/)
  if (!match) return reference
  let node: unknown = scales
  for (const key of match[1]!.split('.')) {
    node = (node as Record<string, unknown>)?.[key]
  }
  const value = (node as { value?: string } | undefined)?.value
  if (!value) throw new Error(`unresolved token reference: ${reference}`)
  return value
}

function luminance(hex: string): number {
  const normalized = hex.replace('#', '')
  const full =
    normalized.length === 3
      ? normalized
          .split('')
          .map((char) => char + char)
          .join('')
      : normalized
  const channels = [0, 2, 4].map((offset) => {
    const value = parseInt(full.slice(offset, offset + 2), 16) / 255
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * channels[0]! + 0.7152 * channels[1]! + 0.0722 * channels[2]!
}

function contrast(foreground: string, background: string): number {
  const a = luminance(foreground)
  const b = luminance(background)
  const [light, dark] = a > b ? [a, b] : [b, a]
  return (light + 0.05) / (dark + 0.05)
}

const role = (name: string) => {
  const token = (semanticColors as Record<string, { value: { base: string; _light?: string } }>)[
    name
  ]
  if (!token) throw new Error(`missing semantic token: ${name}`)
  return token.value
}

describe('token integrity', () => {
  it('every raw color token has a value', () => {
    for (const [name, entry] of Object.entries(scales)) {
      if ('value' in entry) {
        expect((entry as { value: string }).value, name).toBeTruthy()
        continue
      }
      for (const [step, token] of Object.entries(entry as Scale)) {
        expect(token.value, `${name}.${step}`).toMatch(/^#|^rgb|^hsl/)
      }
    }
  })

  it('every semantic color defines a light override', () => {
    for (const [name, token] of Object.entries(semanticColors)) {
      expect(token.value._light, name).toBeTruthy()
    }
  })

  it('label colors define both themes', () => {
    for (const [name, token] of Object.entries(labelColors)) {
      expect(token.value.base, name).toBeTruthy()
      expect(token.value._light, name).toBeTruthy()
    }
  })

  it('radii and durations are well formed', () => {
    expect(radii.md.value).toBe('8px')
    expect(durations.fast.value).toBe('120ms')
  })
})

describe('WCAG 2.1 AA contrast', () => {
  it('body text meets 4.5:1 in both themes', () => {
    for (const mode of ['base', '_light'] as const) {
      const fg = resolveRef(role('fg.default')[mode]!)
      const bg = resolveRef(role('canvas')[mode]!)
      expect(contrast(fg, bg), `fg.default on canvas (${mode})`).toBeGreaterThanOrEqual(4.5)
    }
  })

  it('muted text meets 4.5:1 in both themes', () => {
    for (const mode of ['base', '_light'] as const) {
      const fg = resolveRef(role('fg.muted')[mode]!)
      const bg = resolveRef(role('canvas')[mode]!)
      expect(contrast(fg, bg), `fg.muted on canvas (${mode})`).toBeGreaterThanOrEqual(4.5)
    }
  })

  it('subtle text meets 4.5:1 in both themes', () => {
    for (const mode of ['base', '_light'] as const) {
      const fg = resolveRef(role('fg.subtle')[mode]!)
      const bg = resolveRef(role('canvas')[mode]!)
      expect(contrast(fg, bg), `fg.subtle on canvas (${mode})`).toBeGreaterThanOrEqual(4.5)
    }
  })

  it('text on the accent color meets 4.5:1', () => {
    const fg = resolveRef(role('fg.onAccent').base)
    const bg = resolveRef(role('accent').base)
    expect(contrast(fg, bg)).toBeGreaterThanOrEqual(4.5)
  })
})
