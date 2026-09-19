import type { JSX } from 'solid-js'
import { Show, createUniqueId, splitProps } from 'solid-js'
import { input } from '../../../styled-system/recipes'
import { css, cx } from '../../../styled-system/css'

export interface InputProps extends JSX.InputHTMLAttributes<HTMLInputElement> {
  size?: 'sm' | 'md' | 'lg'
  invalid?: boolean
}

export function Input(props: InputProps): JSX.Element {
  const [local, rest] = splitProps(props, ['size', 'invalid', 'class'])
  return (
    <input
      class={cx(
        input({ size: local.size }),
        local.invalid === true ? css({ borderColor: 'danger' }) : undefined,
        local.class,
      )}
      aria-invalid={local.invalid ? 'true' : undefined}
      {...rest}
    />
  )
}

export interface FieldProps {
  label: string
  description?: string
  error?: string
  required?: boolean
  children: JSX.Element
  class?: string
}

/** Label + control + description/error, wired with ids for screen readers. */
export function Field(props: FieldProps): JSX.Element {
  const id = createUniqueId()
  return (
    <div class={cx(css({ display: 'flex', flexDirection: 'column', gap: '1.5' }), props.class)}>
      <label
        for={id}
        class={css({ fontSize: 'sm', fontWeight: 'medium', color: 'fg.default' })}
      >
        {props.label}
        <Show when={props.required}>
          <span aria-hidden="true" class={css({ color: 'danger', ml: '1' })}>
            *
          </span>
        </Show>
      </label>
      {props.children}
      <Show when={props.description}>
        <p id={`${id}-description`} class={css({ fontSize: 'xs', color: 'fg.muted' })}>
          {props.description}
        </p>
      </Show>
      <Show when={props.error}>
        <p id={`${id}-error`} role="alert" class={css({ fontSize: 'xs', color: 'danger' })}>
          {props.error}
        </p>
      </Show>
    </div>
  )
}

export default Input
