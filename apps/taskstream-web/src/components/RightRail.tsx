import { Show, createSignal } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { useApp } from '../store'
import TaskDetails from './TaskDetails'
import AgentPane from './AgentPane'

const MIN_WIDTH = 300
const MAX_WIDTH = 760

const rail = css({
  position: 'relative',
  display: 'flex',
  flexShrink: '0',
  minH: '0',
  bg: 'canvas',
})

const handle = css({
  position: 'absolute',
  top: '0',
  bottom: '0',
  left: '-3px',
  w: '6px',
  cursor: 'col-resize',
  zIndex: 'raised',
  _hover: { bg: 'accent.subtle' },
  '&[data-dragging]': { bg: 'accent.subtle' },
})

const slot = css({
  display: 'flex',
  flexDirection: 'column',
  flex: '1',
  minW: '0',
  minH: '0',
})

const divider = css({
  borderRightWidth: 'hairline',
  borderRightStyle: 'solid',
  borderRightColor: 'border.subtle',
})

/**
 * The right column. It shows the details pane, the agent pane, or both — the
 * desktop's pane switch — and its left edge drags to resize.
 */
export default function RightRail() {
  const app = useApp()
  const [width, setWidth] = createSignal(430)
  const [dragging, setDragging] = createSignal(false)

  const showDetails = () => app.state.rightPane !== 'agent' && app.selectedTask() !== null
  const showAgent = () => app.state.rightPane !== 'details' || app.selectedTask() === null
  const visible = () => showDetails() || showAgent()

  const startDrag = (event: PointerEvent) => {
    event.preventDefault()
    const startX = event.clientX
    const startWidth = width()
    setDragging(true)

    const move = (moveEvent: PointerEvent) => {
      const delta = startX - moveEvent.clientX
      setWidth(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, startWidth + delta)))
    }
    const stop = () => {
      setDragging(false)
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', stop)
    }

    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', stop)
  }

  return (
    <Show when={visible()}>
      <aside class={rail} style={{ width: `${width()}px` }} aria-label="Task panes">
        <div
          class={handle}
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize pane"
          data-dragging={dragging() ? '' : undefined}
          onPointerDown={startDrag}
        />
        <Show when={showDetails()}>
          <div class={cx(slot, showAgent() ? divider : undefined)}>
            <TaskDetails />
          </div>
        </Show>
        <Show when={showAgent()}>
          <div class={slot}>
            <AgentPane />
          </div>
        </Show>
      </aside>
    </Show>
  )
}
