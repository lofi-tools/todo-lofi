import { For, Show } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { Check, Flag, Repeat } from 'web-design-system/icons'
import { useApp, type Task } from '../store'

const row = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2.5',
  w: 'full',
  px: '2.5',
  py: '2',
  minH: '9',
  borderRadius: 'sm',
  textAlign: 'left',
  cursor: 'pointer',
  transitionProperty: 'background-color',
  transitionDuration: 'fast',
  _hover: { bg: 'surface.hover' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-selected]': { bg: 'surface.hover' },
})

const checkbox = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '4',
  h: '4',
  flexShrink: '0',
  borderRadius: 'full',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.strong',
  color: 'transparent',
  cursor: 'pointer',
  transitionProperty: 'background-color, border-color, color',
  transitionDuration: 'fast',
  _hover: { borderColor: 'accent.text' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-done]': { bg: 'accent', borderColor: 'accent', color: 'fg.onAccent' },
})

const title = css({
  flex: '1',
  minW: '0',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
  fontSize: 'sm',
  color: 'fg.default',
})

const doneTitle = css({ color: 'fg.subtle', textDecoration: 'line-through' })

const tagChip = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1',
  flexShrink: '0',
  px: '1.5',
  h: '5',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.muted',
  whiteSpace: 'nowrap',
})

const signals = css({ display: 'inline-flex', alignItems: 'center', gap: '2', flexShrink: '0' })

const meta = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
  fontVariantNumeric: 'tabular-nums',
  whiteSpace: 'nowrap',
})

const dueStyle = (tone: Task['dueTone']) =>
  tone === 'overdue'
    ? css({ color: 'danger' })
    : tone === 'today'
      ? css({ color: 'accent.text' })
      : undefined

const repeatMark = css({ display: 'inline-flex', color: 'fg.subtle' })

/** Priority is a flag plus the level, quiet at low and accent at high. */
const priorityMark = (priority: Task['priority']) =>
  css({
    display: 'inline-flex',
    alignItems: 'center',
    gap: '1',
    color: priority === 3 ? 'accent.text' : priority === 2 ? 'fg.muted' : 'fg.subtle',
  })

export default function TaskRow(props: { task: Task }) {
  const app = useApp()
  const selected = () => app.selectedTaskId() === props.task.id
  const progress = () => {
    const subtasks = props.task.subtasks
    if (subtasks.length === 0) return null
    return `${subtasks.filter((subtask) => subtask.done).length}/${subtasks.length}`
  }

  return (
    <div
      class={row}
      role="button"
      tabindex="0"
      data-selected={selected() ? '' : undefined}
      aria-current={selected() ? 'true' : undefined}
      onClick={() => app.selectTask(props.task.id)}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          app.selectTask(props.task.id)
        }
      }}
    >
      <button
        type="button"
        class={checkbox}
        data-done={props.task.completed ? '' : undefined}
        aria-label={props.task.completed ? `Reopen ${props.task.title}` : `Complete ${props.task.title}`}
        aria-pressed={props.task.completed}
        onClick={(event) => {
          event.stopPropagation()
          app.toggleComplete(props.task.id)
        }}
      >
        <Check size={11} />
      </button>

      <span class={cx(title, props.task.completed ? doneTitle : undefined)}>{props.task.title}</span>

      <For each={app.tagsFor(props.task)}>
        {(tag) => (
          <span class={tagChip}>
            <span class={css({ w: '1.5', h: '1.5', borderRadius: 'full', bg: tag.color })} />
            {tag.name}
          </span>
        )}
      </For>

      <span class={signals}>
        <Show when={progress()}>
          {(value) => <span class={meta}>{value()}</span>}
        </Show>
        <Show when={props.task.repeat}>
          <span class={repeatMark} title={props.task.repeat ?? undefined}>
            <Repeat size={12} />
          </span>
        </Show>
        <Show when={props.task.priority > 0}>
          <span class={priorityMark(props.task.priority)} title={`Priority ${props.task.priority}`}>
            <Flag size={12} />
            <span class={meta}>P{props.task.priority}</span>
          </span>
        </Show>
        <Show when={props.task.due}>
          {(due) => <span class={cx(meta, dueStyle(props.task.dueTone))}>{due()}</span>}
        </Show>
      </span>
    </div>
  )
}
