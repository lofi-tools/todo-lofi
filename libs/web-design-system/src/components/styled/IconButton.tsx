import type { JSX } from 'solid-js'
import { splitProps } from 'solid-js'
import { iconButton } from '../../../styled-system/recipes'
import { cx } from '../../../styled-system/css'

export interface IconButtonProps extends JSX.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'ghost' | 'outline' | 'solid'
  size?: 'xs' | 'sm' | 'md' | 'lg'
  /** Required: icon-only controls need an accessible name (spec §9). */
  'aria-label': string
}

export function IconButton(props: IconButtonProps): JSX.Element {
  const [local, rest] = splitProps(props, ['variant', 'size', 'class'])
  return (
    <button
      type="button"
      class={cx(
        iconButton({ variant: local.variant, size: local.size }),
        local.class,
      )}
      {...rest}
    />
  )
}

export default IconButton
