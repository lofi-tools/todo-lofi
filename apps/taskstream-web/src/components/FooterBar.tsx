import { For, Show } from 'solid-js'
import { css } from 'styled-system/css'
import { Sparkles } from 'web-design-system/icons'
import { useApp, type RightPane } from '../store'
import NotificationsPanel from './NotificationsPanel'

const footer = css({
  display: 'flex',
  alignItems: 'center',
  gap: '3',
  h: '10',
  flexShrink: '0',
  px: '3',
  borderTopWidth: 'hairline',
  borderTopStyle: 'solid',
  borderTopColor: 'border.subtle',
  bg: 'surface.subtle',
})

const statusText = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
  whiteSpace: 'nowrap',
})

const spacer = css({ flex: '1' })

const segment = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '0.5',
  p: '0.5',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.elevated',
})

const segmentButton = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1',
  h: '6',
  px: '2',
  borderRadius: 'xs',
  fontSize: '2xs',
  color: 'fg.muted',
  cursor: 'pointer',
  transitionProperty: 'color, background-color',
  transitionDuration: 'fast',
  _hover: { color: 'fg.default' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-active]': { color: 'fg.default', bg: 'surface.hover' },
})

const PANES: { id: RightPane; label: string }[] = [
  { id: 'details', label: 'Details' },
  { id: 'split', label: 'Split' },
  { id: 'agent', label: 'Agent' },
]

/** The window footer: what the list holds on the left, the pane switch and
 *  notifications on the right. */
export default function FooterBar() {
  const app = useApp()
  const open = () => app.visibleTasks().length
  const done = () => app.completedTasks().length
  const blocked = () => app.state.tasks.filter((task) => !task.completed && app.isBlocked(task)).length
  const isTasks = () =>
    app.state.destination.kind === 'all' || app.state.destination.kind === 'tag'

  return (
    <footer class={footer}>
      <Show when={isTasks()} fallback={<span class={statusText}>{app.destinationLabel()}</span>}>
        <span class={statusText}>
          {open()} open · {done()} done
        </span>
        <Show when={blocked() > 0}>
          <span class={statusText}>· {blocked()} blocked, hidden</span>
        </Show>
      </Show>

      <div class={spacer} />

      <Show when={isTasks()}>
        <div class={segment} role="group" aria-label="Right pane">
          <For each={PANES}>
            {(pane) => (
              <button
                type="button"
                class={segmentButton}
                data-active={app.state.rightPane === pane.id ? '' : undefined}
                aria-pressed={app.state.rightPane === pane.id}
                onClick={() => app.setRightPane(pane.id)}
              >
                <Show when={pane.id !== 'details'}>
                  <Sparkles size={11} />
                </Show>
                {pane.label}
              </button>
            )}
          </For>
        </div>
      </Show>

      <NotificationsPanel />
    </footer>
  )
}
