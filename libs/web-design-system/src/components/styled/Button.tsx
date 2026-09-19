import type { JSX } from 'solid-js'
import { splitProps } from 'solid-js'
import { button } from '../../../styled-system/recipes'
import { cx } from '../../../styled-system/css'

export interface ButtonProps extends JSX.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger' | 'subtle'
  size?: 'xs' | 'sm' | 'md' | 'lg'
  fullWidth?: boolean
}

export function Button(props: ButtonProps): JSX.Element {
  const [local, rest] = splitProps(props, ['variant', 'size', 'fullWidth', 'class'])
  return (
    <button
      type="button"
      class={cx(
        button({
          variant: local.variant,
          size: local.size,
          fullWidth: local.fullWidth,
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export default Button
