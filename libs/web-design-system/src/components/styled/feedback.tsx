import type { JSX } from 'solid-js'
import { Show, splitProps } from 'solid-js'
import { kbd, statusDot as statusDotRecipe } from '../../../styled-system/recipes'
import { css, cx } from '../../../styled-system/css'

export interface KbdProps extends JSX.HTMLAttributes<HTMLElement> {}

/** A keycap, e.g. the ⌘K hint in the navbar. */
export function Kbd(props: KbdProps): JSX.Element {
  const [local, rest] = splitProps(props, ['class'])
  return <kbd class={cx(kbd(), local.class)} {...rest} />
}

export type StatusTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger' | 'info'

export interface StatusDotProps extends JSX.HTMLAttributes<HTMLSpanElement> {
  tone?: StatusTone
  pulse?: boolean
}

export function StatusDot(props: StatusDotProps): JSX.Element {
  const [local, rest] = splitProps(props, ['tone', 'pulse', 'class'])
  return (
    <span
      class={cx(statusDotRecipe({ tone: local.tone }), local.class)}
      classList={{ 'wds-pulse': local.pulse }}
      {...rest}
    />
  )
}

export interface SpinnerProps extends JSX.HTMLAttributes<HTMLSpanElement> {
  size?: 'sm' | 'md' | 'lg'
  label?: string
}

export function Spinner(props: SpinnerProps): JSX.Element {
  const [local, rest] = splitProps(props, ['size', 'label', 'class'])
  const size = () => (local.size === 'sm' ? '12px' : local.size === 'lg' ? '24px' : '16px')
  return (
    <span
      role="status"
      aria-label={local.label ?? 'Loading'}
      class={cx(
        css({
          display: 'inline-block',
          w: size(),
          h: size(),
          borderRadius: 'full',
          borderWidth: '2px',
          borderStyle: 'solid',
          borderColor: 'border',
          borderTopColor: 'accent',
          animation: 'spin 0.7s linear infinite',
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export interface SkeletonProps extends JSX.HTMLAttributes<HTMLDivElement> {
  width?: string
  height?: string
  radius?: 'sm' | 'md' | 'full'
}

export function Skeleton(props: SkeletonProps): JSX.Element {
  const [local, rest] = splitProps(props, ['width', 'height', 'radius', 'class'])
  return (
    <div
      aria-hidden="true"
      class={cx(
        css({
          w: local.width ?? '100%',
          h: local.height ?? '1rem',
          borderRadius: local.radius ?? 'sm',
          bg: 'surface.hover',
          animation: 'pulse 1.6s ease-in-out infinite',
        }),
        local.class,
      )}
      {...rest}
    />
  )
}

export type AlertTone = 'neutral' | 'info' | 'success' | 'warning' | 'danger'

export interface AlertProps extends JSX.HTMLAttributes<HTMLDivElement> {
  tone?: AlertTone
  title?: string
  icon?: JSX.Element
}

export function Alert(props: AlertProps): JSX.Element {
  const [local, rest] = splitProps(props, ['tone', 'title', 'icon', 'class', 'children'])
  const tones: Record<AlertTone, object> = {
    neutral: { bg: 'surface.subtle', borderColor: 'border' },
    info: { bg: 'accent.subtle', borderColor: 'accent' },
    success: { bg: 'success.subtle', borderColor: 'success' },
    warning: { bg: 'warning.subtle', borderColor: 'warning' },
    danger: { bg: 'danger.subtle', borderColor: 'danger' },
  }
  return (
    <div
      role="note"
      class={cx(
        css({
          display: 'flex',
          gap: '3',
          p: '4',
          borderRadius: 'md',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          color: 'fg.default',
          fontSize: 'sm',
          lineHeight: 'relaxed',
          ...tones[local.tone ?? 'neutral'],
        }),
        local.class,
      )}
      {...rest}
    >
      <Show when={local.icon}>{local.icon}</Show>
      <div class={css({ minW: '0' })}>
        <Show when={local.title}>
          <div class={css({ fontWeight: 'semibold', mb: '1' })}>{local.title}</div>
        </Show>
        {local.children}
      </div>
    </div>
  )
}
