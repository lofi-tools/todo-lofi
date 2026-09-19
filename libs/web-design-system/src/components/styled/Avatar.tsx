import type { JSX } from 'solid-js'
import { For, Show, splitProps } from 'solid-js'
import { avatar } from '../../../styled-system/recipes'
import { css, cx } from '../../../styled-system/css'

export interface AvatarProps extends JSX.HTMLAttributes<HTMLSpanElement> {
  name: string
  src?: string
  size?: 'xs' | 'sm' | 'md' | 'lg' | 'xl'
}

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean)
  if (parts.length === 0) return '?'
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase()
  return `${parts[0]![0]}${parts[parts.length - 1]![0]}`.toUpperCase()
}

export function Avatar(props: AvatarProps): JSX.Element {
  const [local, rest] = splitProps(props, ['name', 'src', 'size', 'class'])
  return (
    <span
      class={cx(avatar({ size: local.size }), css({ position: 'relative' }), local.class)}
      title={local.name}
      {...rest}
    >
      <Show
        when={local.src}
        fallback={<span aria-hidden="true">{initials(local.name)}</span>}
      >
        <img
          src={local.src}
          alt={local.name}
          class={css({ w: 'full', h: 'full', objectFit: 'cover' })}
        />
      </Show>
    </span>
  )
}

export interface AvatarGroupProps {
  names: string[]
  size?: AvatarProps['size']
  max?: number
  class?: string
}

/** Overlapping avatars with a `+N` overflow chip for assignee stacks. */
export function AvatarGroup(props: AvatarGroupProps): JSX.Element {
  const shown = () => props.names.slice(0, props.max ?? 4)
  const overflow = () => Math.max(0, props.names.length - shown().length)
  return (
    <span class={cx(css({ display: 'inline-flex', alignItems: 'center' }), props.class)}>
      <For each={shown()}>
        {(name, index) => (
          <span
            class={css({
              ml: index() === 0 ? '0' : '-1.5',
              ringWidth: '2px',
              ringColor: 'canvas',
              borderRadius: 'full',
            })}
          >
            <Avatar name={name} size={props.size ?? 'sm'} />
          </span>
        )}
      </For>
      <Show when={overflow() > 0}>
        <span
          class={cx(
            avatar({ size: props.size ?? 'sm' }),
            css({ ml: '-1.5', ringWidth: '2px', ringColor: 'canvas' }),
          )}
          title={`${overflow()} more`}
        >
          +{overflow()}
        </span>
      </Show>
    </span>
  )
}

export default Avatar
