import type { JSX } from 'solid-js'
import { splitProps } from 'solid-js'
import { css, cx } from '../../../styled-system/css'
import { card } from '../../../styled-system/recipes'
import { grid, hstack, vstack } from '../../../styled-system/patterns'

type Gap = '0' | '1' | '1.5' | '2' | '3' | '4' | '5' | '6' | '8' | '10' | '12' | '16'
type Align = 'start' | 'center' | 'end' | 'stretch' | 'baseline'
type Justify = 'start' | 'center' | 'end' | 'between'

const justifyValue = (justify?: Justify) =>
  justify === 'between' ? 'space-between' : justify

export type BoxProps = JSX.HTMLAttributes<HTMLDivElement>

/** Bare styled element — the escape hatch for one-off layout. */
export function Box(props: BoxProps): JSX.Element {
  const [local, rest] = splitProps(props, ['class'])
  return <div class={cx(css({ minW: '0' }), local.class)} {...rest} />
}

export interface StackProps extends JSX.HTMLAttributes<HTMLDivElement> {
  gap?: Gap
  align?: Align
  justify?: Justify
  wrap?: boolean
}

export function HStack(props: StackProps): JSX.Element {
  const [local, rest] = splitProps(props, ['gap', 'align', 'justify', 'wrap', 'class'])
  return (
    <div
      class={cx(
        hstack({
          gap: local.gap ?? '2',
          alignItems: local.align,
          justifyContent: justifyValue(local.justify),
          flexWrap: local.wrap ? 'wrap' : undefined,
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export function VStack(props: StackProps): JSX.Element {
  const [local, rest] = splitProps(props, ['gap', 'align', 'justify', 'wrap', 'class'])
  return (
    <div
      class={cx(
        vstack({
          gap: local.gap ?? '2',
          alignItems: local.align,
          justifyContent: justifyValue(local.justify),
          flexWrap: local.wrap ? 'wrap' : undefined,
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export interface GridProps extends JSX.HTMLAttributes<HTMLDivElement> {
  columns?: number
  minChildWidth?: string
  gap?: Gap
}

export function Grid(props: GridProps): JSX.Element {
  const [local, rest] = splitProps(props, ['columns', 'minChildWidth', 'gap', 'class'])
  return (
    <div
      class={cx(
        grid({
          columns: local.columns,
          minChildWidth: local.minChildWidth,
          gap: local.gap ?? '6',
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export interface ContainerProps extends JSX.HTMLAttributes<HTMLDivElement> {
  size?: 'sm' | 'md' | 'lg' | 'xl'
}

const CONTAINER_WIDTHS = {
  sm: '640px',
  md: '840px',
  lg: '1024px',
  xl: '1200px',
} as const

/** Centered content column — Linear's ~1200px measure at `xl`. */
export function Container(props: ContainerProps): JSX.Element {
  const [local, rest] = splitProps(props, ['size', 'class'])
  return (
    <div
      class={cx(
        css({
          w: '100%',
          maxW: CONTAINER_WIDTHS[local.size ?? 'xl'],
          mx: 'auto',
          px: ['5', '5', '8'],
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export interface CardProps extends JSX.HTMLAttributes<HTMLDivElement> {
  padding?: 'none' | 'sm' | 'md' | 'lg'
  interactive?: boolean
}

/** Surface container used by marketing, docs, and gallery cards. */
export function Card(props: CardProps): JSX.Element {
  const [local, rest] = splitProps(props, ['padding', 'interactive', 'class'])
  return (
    <div
      class={cx(
        card({ padding: local.padding, interactive: local.interactive }),
        local.class,
      )}
      {...rest}
    />
  )
}

export function Separator(
  props: JSX.HTMLAttributes<HTMLDivElement> & { vertical?: boolean },
): JSX.Element {
  const [local, rest] = splitProps(props, ['vertical', 'class'])
  return (
    <div
      role="separator"
      aria-orientation={local.vertical ? 'vertical' : 'horizontal'}
      class={cx(
        css(
          local.vertical
            ? { flexShrink: '0', w: '1px', h: 'full', bg: 'border' }
            : { flexShrink: '0', h: '1px', w: 'full', bg: 'border' },
        ),
        local.class,
      )}
      {...rest}
    />
  )
}
