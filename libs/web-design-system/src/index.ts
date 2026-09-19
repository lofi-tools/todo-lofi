export { preset } from './preset'
export * from './tokens'
export * from './components/styled'
export { cn } from './lib/cn'
export type { ClassValue } from './lib/cn'

/**
 * The headless layer shares many names with the styled layer (`Dialog`,
 * `Menu`, `Tooltip`, …), so it is exported under a namespace rather than
 * flattened — importing it directly would silently shadow one or the other.
 * Consumers who want the unstyled primitives use `web-design-system/headless`.
 */
export * as headless from './components/headless'
