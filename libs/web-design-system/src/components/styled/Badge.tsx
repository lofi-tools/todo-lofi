import type { JSX } from 'solid-js'
import { splitProps } from 'solid-js'
import { badge } from '../../../styled-system/recipes'
import { cx } from '../../../styled-system/css'

export type BadgeTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger' | 'info'

export interface BadgeProps extends JSX.HTMLAttributes<HTMLSpanElement> {
  tone?: BadgeTone
  variant?: 'subtle' | 'solid' | 'outline'
}

export function Badge(props: BadgeProps): JSX.Element {
  const [local, rest] = splitProps(props, ['tone', 'variant', 'class'])
  return (
    <span
      class={cx(
        badge({ tone: local.tone, variant: local.variant }),
        local.class,
      )}
      {...rest}
    />
  )
}

export default Badge
