import { For, Show, createSignal } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { Check, Flag, Lock, Repeat, X } from 'web-design-system/icons'
import { useApp, type DueTone, type Priority } from '../store'

const pane = css({
  display: 'flex',
  flexDirection: 'column',
  flex: '1',
  minH: '0',
  bg: 'canvas',
})

const head = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  flexShrink: '0',
  h: '12',
  px: '4',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
})

const eyebrow = css({
  flex: '1',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const closeButton = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '7',
  h: '7',
  borderRadius: 'sm',
  color: 'fg.subtle',
  cursor: 'pointer',
  _hover: { color: 'fg.default', bg: 'surface.hover' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const scroll = css({
  flex: '1',
  minH: '0',
  overflowY: 'auto',
  px: '4',
  py: '4',
  display: 'flex',
  flexDirection: 'column',
  gap: '5',
})

const titleInput = css({
  w: 'full',
  bg: 'transparent',
  borderWidth: '0',
  color: 'fg.default',
  fontSize: 'lg',
  fontWeight: 'semibold',
  lineHeight: 'snug',
  _focusVisible: { outline: 'none' },
})

const meta = css({ display: 'flex', flexDirection: 'column', gap: '2' })

const metaRow = css({ display: 'flex', alignItems: 'baseline', gap: '3', minH: '7' })

const metaLabel = css({
  w: '20',
  flexShrink: '0',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const metaValue = css({ display: 'flex', alignItems: 'center', gap: '1.5', flexWrap: 'wrap', flex: '1', minW: '0' })

const chip = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1',
  h: '6',
  px: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  fontSize: 'xs',
  color: 'fg.muted',
  cursor: 'pointer',
  transitionProperty: 'color, border-color, background-color',
  transitionDuration: 'fast',
  _hover: { color: 'fg.default', borderColor: 'border.strong' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-active]': { color: 'fg.default', borderColor: 'accent', bg: 'accent.subtle' },
})

const textInput = css({
  w: 'full',
  h: '8',
  px: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  color: 'fg.default',
  fontSize: 'xs',
  _focusVisible: { outline: 'none', borderColor: 'border.strong' },
  _placeholder: { color: 'fg.subtle' },
})

const notes = css({
  w: 'full',
  minH: '24',
  p: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  color: 'fg.default',
  fontSize: 'sm',
  lineHeight: 'relaxed',
  resize: 'vertical',
  _focusVisible: { outline: 'none', borderColor: 'border.strong' },
  _placeholder: { color: 'fg.subtle' },
})

const block = css({ display: 'flex', flexDirection: 'column', gap: '2' })

const blockHead = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const subtaskRow = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  px: '1',
  py: '1',
  borderRadius: 'xs',
})

const subtaskCheck = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '3.5',
  h: '3.5',
  flexShrink: '0',
  borderRadius: 'xs',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.strong',
  color: 'transparent',
  cursor: 'pointer',
  '&[data-done]': { bg: 'accent', borderColor: 'accent', color: 'fg.onAccent' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const subtaskTitle = css({
  flex: '1',
  minW: '0',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
  fontSize: 'xs',
  color: 'fg.default',
  cursor: 'pointer',
  '&[data-done]': { color: 'fg.subtle', textDecoration: 'line-through' },
})

const blockedRow = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  px: '2',
  h: '7',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'dashed',
  borderColor: 'border.strong',
  fontSize: 'xs',
  color: 'fg.muted',
})

const DUE_PRESETS: { label: string; due: string; tone: DueTone }[] = [
  { label: 'Today', due: 'today', tone: 'today' },
  { label: 'Tomorrow', due: 'tomorrow', tone: 'soon' },
  { label: 'In 3 days', due: 'in 3d', tone: 'soon' },
  { label: 'Next week', due: 'in 7d', tone: 'later' },
]

const REPEATS = ['every day', 'every 3 days', 'every week', 'every 2 weeks', 'every month'] as const

const PRIORITIES: { value: Priority; label: string }[] = [
  { value: 0, label: 'None' },
  { value: 1, label: 'P1' },
  { value: 2, label: 'P2' },
  { value: 3, label: 'P3' },
]

const empty = css({
  display: 'flex',
  flex: '1',
  flexDirection: 'column',
  alignItems: 'center',
  justifyContent: 'center',
  gap: '2',
  color: 'fg.subtle',
  fontSize: 'sm',
})

export default function TaskDetails() {
  const app = useApp()
  const [subtaskDraft, setSubtaskDraft] = createSignal('')

  return (
    <section class={pane} aria-label="Task details">
      <Show
        when={app.selectedTask()}
        fallback={
          <div class={empty}>
            <span>No task selected.</span>
            <span class={css({ fontSize: 'xs' })}>Pick a row to see its details.</span>
          </div>
        }
      >
        {(task) => (
          <>
            <header class={head}>
              <span class={eyebrow}>
                {task().completed ? 'Completed task' : 'Open task'}
              </span>
              <button
                type="button"
                class={closeButton}
                aria-label="Close details"
                onClick={() => app.selectTask(null)}
              >
                <X size={14} />
              </button>
            </header>

            <div class={scroll}>
              <input
                class={titleInput}
                value={task().title}
                aria-label="Task title"
                onInput={(event) => app.updateTask(task().id, { title: event.currentTarget.value })}
              />

              <div class={meta}>
                <div class={metaRow}>
                  <span class={metaLabel}>Status</span>
                  <span class={metaValue}>
                    <button
                      type="button"
                      class={chip}
                      data-active={task().completed ? '' : undefined}
                      aria-pressed={task().completed}
                      onClick={() => app.toggleComplete(task().id)}
                    >
                      <Check size={11} />
                      {task().completed ? 'Completed' : 'Mark complete'}
                    </button>
                  </span>
                </div>

                <div class={metaRow}>
                  <span class={metaLabel}>Due</span>
                  <span class={metaValue}>
                    <For each={DUE_PRESETS}>
                      {(preset) => (
                        <button
                          type="button"
                          class={chip}
                          data-active={task().due === preset.due ? '' : undefined}
                          onClick={() =>
                            app.updateTask(task().id, {
                              due: task().due === preset.due ? null : preset.due,
                              dueTone: task().due === preset.due ? null : preset.tone,
                            })
                          }
                        >
                          {preset.label}
                        </button>
                      )}
                    </For>
                    <Show when={task().due}>
                      <button
                        type="button"
                        class={chip}
                        onClick={() => app.updateTask(task().id, { due: null, dueTone: null })}
                      >
                        Clear
                      </button>
                    </Show>
                  </span>
                </div>

                <div class={metaRow}>
                  <span class={metaLabel}>Priority</span>
                  <span class={metaValue}>
                    <For each={PRIORITIES}>
                      {(priority) => (
                        <button
                          type="button"
                          class={chip}
                          data-active={task().priority === priority.value ? '' : undefined}
                          onClick={() => app.updateTask(task().id, { priority: priority.value })}
                        >
                          <Show when={priority.value > 0}>
                            <Flag size={11} />
                          </Show>
                          {priority.label}
                        </button>
                      )}
                    </For>
                  </span>
                </div>

                <div class={metaRow}>
                  <span class={metaLabel}>Repeat</span>
                  <span class={metaValue}>
                    <button
                      type="button"
                      class={chip}
                      data-active={task().repeat === null ? '' : undefined}
                      onClick={() => app.updateTask(task().id, { repeat: null })}
                    >
                      <Repeat size={11} />
                      None
                    </button>
                    <For each={REPEATS}>
                      {(rule) => (
                        <button
                          type="button"
                          class={chip}
                          data-active={task().repeat === rule ? '' : undefined}
                          onClick={() =>
                            app.updateTask(task().id, {
                              repeat: task().repeat === rule ? null : rule,
                            })
                          }
                        >
                          {rule}
                        </button>
                      )}
                    </For>
                  </span>
                </div>

                <div class={metaRow}>
                  <span class={metaLabel}>Tags</span>
                  <span class={metaValue}>
                    <For each={app.state.tags}>
                      {(tag) => (
                        <button
                          type="button"
                          class={chip}
                          data-active={task().tagIds.includes(tag.id) ? '' : undefined}
                          aria-pressed={task().tagIds.includes(tag.id)}
                          onClick={() => app.toggleTag(task().id, tag.id)}
                        >
                          <span class={css({ w: '1.5', h: '1.5', borderRadius: 'full', bg: tag.color })} />
                          {tag.name}
                        </button>
                      )}
                    </For>
                  </span>
                </div>

                <Show when={task().blockedBy.length > 0}>
                  <div class={metaRow}>
                    <span class={metaLabel}>Blocked by</span>
                    <span class={css({ display: 'flex', flexDirection: 'column', gap: '1.5', flex: '1' })}>
                      <For each={app.blockedByTitles(task())}>
                        {(title) => (
                          <span class={blockedRow}>
                            <Lock size={11} />
                            {title}
                          </span>
                        )}
                      </For>
                    </span>
                  </div>
                </Show>
              </div>

              <div class={block}>
                <span class={blockHead}>Notes</span>
                <textarea
                  class={notes}
                  value={task().notes}
                  placeholder="Add notes…"
                  aria-label="Notes"
                  onInput={(event) => app.updateTask(task().id, { notes: event.currentTarget.value })}
                />
              </div>

              <div class={block}>
                <span class={blockHead}>
                  Subtasks
                  <Show when={task().subtasks.length > 0}>
                    <span class={css({ color: 'fg.subtle' })}>
                      {task().subtasks.filter((subtask) => subtask.done).length}/{task().subtasks.length}
                    </span>
                  </Show>
                </span>
                <For each={task().subtasks}>
                  {(subtask) => (
                    <span class={subtaskRow}>
                      <button
                        type="button"
                        class={subtaskCheck}
                        data-done={subtask.done ? '' : undefined}
                        aria-label={subtask.done ? `Reopen ${subtask.title}` : `Complete ${subtask.title}`}
                        aria-pressed={subtask.done}
                        onClick={() => app.toggleSubtask(task().id, subtask.id)}
                      >
                        <Check size={10} />
                      </button>
                      <span
                        class={subtaskTitle}
                        data-done={subtask.done ? '' : undefined}
                        onClick={() => app.toggleSubtask(task().id, subtask.id)}
                      >
                        {subtask.title}
                      </span>
                    </span>
                  )}
                </For>
                <input
                  class={textInput}
                  value={subtaskDraft()}
                  placeholder="Add a sub-task, then Enter"
                  aria-label="Add a sub-task"
                  onInput={(event) => setSubtaskDraft(event.currentTarget.value)}
                  onKeyDown={(event) => {
                    if (event.key !== 'Enter') return
                    app.addSubtask(task().id, subtaskDraft())
                    setSubtaskDraft('')
                  }}
                />
              </div>
            </div>
          </>
        )}
      </Show>
    </section>
  )
}
