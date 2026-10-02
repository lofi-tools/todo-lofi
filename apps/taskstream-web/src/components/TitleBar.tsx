import { Show } from 'solid-js'
import { css } from 'styled-system/css'
import { ChevronLeft, ChevronRight, Repeat, Search } from 'web-design-system/icons'
import { IconButton, ThemeToggle } from 'web-design-system/components'
import { useApp } from '../store'

const bar = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  h: '12',
  flexShrink: '0',
  px: '3',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
  bg: 'surface.subtle',
})

const group = css({ display: 'inline-flex', alignItems: 'center', gap: '1' })

const label = css({
  flex: '1',
  minW: '0',
  textAlign: 'center',
  fontSize: 'xs',
  fontWeight: 'medium',
  color: 'fg.muted',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
})

const searchHint = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '2',
  h: '8',
  px: '3',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border',
  bg: 'surface.elevated',
  color: 'fg.subtle',
  fontSize: 'xs',
  cursor: 'pointer',
  transitionProperty: 'border-color, color',
  transitionDuration: 'fast',
  _hover: { borderColor: 'border.strong', color: 'fg.muted' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const badge = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  minW: '4',
  h: '4',
  px: '1',
  borderRadius: 'full',
  bg: 'warning',
  color: 'canvas',
  fontSize: '2xs',
  fontWeight: 'semibold',
})

const historyButton = css({
  '&[data-active]': { bg: 'surface.hover', color: 'fg.default' },
})

/** The app's own title bar: history navigation over the whole window. */
export default function TitleBar() {
  const app = useApp()
  const failed = () => app.state.notifications.filter((notice) => notice.tone === 'warning').length

  return (
    <header class={bar}>
      <div class={group}>
        <IconButton
          aria-label="Previous task"
          variant="ghost"
          size="sm"
          disabled={!app.canGoBack()}
          onClick={app.goBack}
        >
          <ChevronLeft size={15} />
        </IconButton>
        <IconButton
          aria-label="Next task"
          variant="ghost"
          size="sm"
          disabled={!app.canGoForward()}
          onClick={app.goForward}
        >
          <ChevronRight size={15} />
        </IconButton>
        <IconButton
          aria-label="Sync history"
          variant="ghost"
          size="sm"
          class={historyButton}
          data-active={app.state.syncHistoryOpen ? '' : undefined}
          onClick={() => {
            if (!app.state.syncHistoryOpen) app.setDestination({ kind: 'sync' })
            else app.setDestination({ kind: 'all' })
          }}
        >
          <span class={css({ display: 'inline-flex', alignItems: 'center', gap: '1', position: 'relative' })}>
            <Repeat size={14} />
            <Show when={failed() > 0}>
              <span class={badge}>{failed()}</span>
            </Show>
          </span>
        </IconButton>
      </div>

      <span class={label}>{app.destinationLabel()}</span>

      <div class={group}>
        <button
          type="button"
          class={searchHint}
          onClick={app.toggleCommand}
          aria-label="Open the command palette"
        >
          <Search size={13} />
          <span class={css({ display: { base: 'none', md: 'inline' } })}>Search</span>
          <kbd class={css({ fontFamily: 'mono', fontSize: '2xs', color: 'fg.subtle' })}>⌘K</kbd>
        </button>
        <ThemeToggle />
      </div>
    </header>
  )
}
