import { Show, type JSX } from 'solid-js'
import { css } from 'styled-system/css'

export const view = css({
  display: 'flex',
  flexDirection: 'column',
  flex: '1',
  minW: '0',
  minH: '0',
  bg: 'canvas',
})

export const header = css({
  display: 'flex',
  alignItems: 'center',
  gap: '3',
  flexShrink: '0',
  h: '12',
  px: '5',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
})

export const headerTitle = css({
  fontSize: 'sm',
  fontWeight: 'semibold',
  color: 'fg.default',
})

export const headerSubtitle = css({
  fontSize: 'xs',
  color: 'fg.subtle',
  minW: '0',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
})

export const body = css({
  flex: '1',
  minH: '0',
  overflowY: 'auto',
  px: '5',
  py: '5',
  display: 'flex',
  flexDirection: 'column',
  gap: '4',
})

export const card = css({
  display: 'flex',
  flexDirection: 'column',
  gap: '3',
  p: '4',
  borderRadius: 'lg',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
})

export const cardHead = css({ display: 'flex', alignItems: 'center', gap: '3' })

export const cardTitle = css({ fontSize: 'sm', fontWeight: 'medium', color: 'fg.default' })

export const cardMeta = css({ fontSize: 'xs', color: 'fg.subtle', flex: '1', minW: '0' })

export const rowBetween = css({
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: '3',
})

export const gridTwo = css({
  display: 'grid',
  gridTemplateColumns: 'repeat(auto-fill, minmax(320px, 1fr))',
  gap: '4',
})

export const field = css({
  display: 'flex',
  flexDirection: 'column',
  gap: '1',
  padding: '3',
  borderRadius: 'md',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.elevated',
})

export const fieldLabel = css({ fontSize: 'xs', color: 'fg.default' })

export const fieldHint = css({ fontSize: '2xs', color: 'fg.subtle' })

export const row = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  flexWrap: 'wrap',
})

export const chip = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1',
  h: '6',
  px: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.elevated',
  fontSize: 'xs',
  color: 'fg.muted',
  cursor: 'pointer',
  transitionProperty: 'color, border-color, background-color',
  transitionDuration: 'fast',
  _hover: { color: 'fg.default', borderColor: 'border.strong' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-active]': { color: 'fg.default', borderColor: 'accent', bg: 'accent.subtle' },
})

export const action = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1.5',
  h: '7',
  px: '3',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.strong',
  bg: 'surface.elevated',
  fontSize: 'xs',
  color: 'fg.default',
  cursor: 'pointer',
  transitionProperty: 'color, border-color, background-color',
  transitionDuration: 'fast',
  _hover: { borderColor: 'accent', color: 'fg.default' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-muted]': { color: 'fg.subtle', borderColor: 'border.subtle' },
  '&[data-danger]': { color: 'danger', borderColor: 'border.subtle' },
})

export const textInput = css({
  w: 'full',
  h: '8',
  px: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'canvas',
  color: 'fg.default',
  fontSize: 'xs',
  _focusVisible: { outline: 'none', borderColor: 'border.strong' },
  _placeholder: { color: 'fg.subtle' },
})

export const note = css({ fontSize: 'xs', color: 'fg.muted', lineHeight: 'relaxed' })

export const divider = css({
  borderTopWidth: 'hairline',
  borderTopStyle: 'solid',
  borderTopColor: 'border.subtle',
})

/** A destination panel: a header line and a scrolling body of cards. */
export default function ViewShell(props: {
  title: string
  subtitle?: string
  actions?: JSX.Element
  children: JSX.Element
}) {
  return (
    <div class={view}>
      <header class={header}>
        <span class={headerTitle}>{props.title}</span>
        <Show when={props.subtitle}>
          <span class={headerSubtitle}>{props.subtitle}</span>
        </Show>
        <span class={css({ flex: '1' })} />
        {props.actions}
      </header>
      <div class={body}>{props.children}</div>
    </div>
  )
}
