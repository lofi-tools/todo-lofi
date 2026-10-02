import { For, Show, createMemo, createSignal } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { ChevronDown, ChevronRight, Search } from 'web-design-system/icons'
import { useApp } from '../store'
import TaskRow from './TaskRow'

const pane = css({
  display: 'flex',
  flexDirection: 'column',
  flex: '1',
  minW: '0',
  minH: '0',
  borderRightWidth: 'hairline',
  borderRightStyle: 'solid',
  borderRightColor: 'border.subtle',
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

const heading = css({ fontSize: 'md', fontWeight: 'semibold', color: 'fg.default', m: '0' })

const countBadge = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
  fontVariantNumeric: 'tabular-nums',
})

const filterField = css({
  display: 'flex',
  alignItems: 'center',
  gap: '1.5',
  h: '7',
  px: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  color: 'fg.subtle',
  _focusWithin: { borderColor: 'border.strong' },
})

const filterInput = css({
  w: '32',
  bg: 'transparent',
  borderWidth: '0',
  color: 'fg.default',
  fontSize: 'xs',
  _focusVisible: { outline: 'none' },
  _placeholder: { color: 'fg.subtle' },
})

const composer = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2.5',
  flexShrink: '0',
  px: '2.5',
  py: '2',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
})

const composerInput = css({
  flex: '1',
  minW: '0',
  h: '8',
  px: '2',
  bg: 'transparent',
  borderWidth: '0',
  color: 'fg.default',
  fontSize: 'sm',
  _focusVisible: { outline: 'none' },
  _placeholder: { color: 'fg.subtle' },
})

const scroll = css({
  flex: '1',
  minH: '0',
  overflowY: 'auto',
  px: '2',
  py: '3',
})

const sectionGroup = css({ display: 'flex', flexDirection: 'column', gap: '0.5', mb: '4' })

const sectionHead = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  h: '7',
  px: '2.5',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const sectionTitle = css({ flex: '1' })

const toggle = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1',
  color: 'fg.subtle',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  cursor: 'pointer',
  _hover: { color: 'fg.default' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const empty = css({
  display: 'flex',
  flexDirection: 'column',
  alignItems: 'center',
  gap: '2',
  py: '16',
  color: 'fg.subtle',
  fontSize: 'sm',
  textAlign: 'center',
})

const overdueTone = css({ color: 'danger' })
const todayTone = css({ color: 'accent.text' })

/** The list pane: the destination's open tasks in due sections, then completed. */
export default function TaskList() {
  const app = useApp()
  const [filter, setFilter] = createSignal('')
  const [draft, setDraft] = createSignal('')
  const [showCompleted, setShowCompleted] = createSignal(false)

  const matches = (text: string) => {
    const needle = filter().trim().toLowerCase()
    return needle.length === 0 || text.toLowerCase().includes(needle)
  }

  const sections = createMemo(() =>
    app
      .sections()
      .map((section) => ({ ...section, tasks: section.tasks.filter((task) => matches(task.title)) }))
      .filter((section) => section.tasks.length > 0),
  )

  const completed = createMemo(() =>
    app.completedTasks().filter((task) => matches(task.title)),
  )

  const openCount = createMemo(() => sections().reduce((total, section) => total + section.tasks.length, 0))
  const isEmpty = createMemo(() => openCount() === 0 && completed().length === 0)

  const commit = () => {
    app.addTask(draft())
    setDraft('')
  }

  return (
    <section class={pane} aria-label={app.destinationLabel()}>
      <header class={head}>
        <h1 class={heading}>{app.destinationLabel()}</h1>
        <span class={countBadge}>{openCount()}</span>
        <span class={css({ flex: '1' })} />
        <label class={filterField}>
          <Search size={12} />
          <input
            class={filterInput}
            value={filter()}
            placeholder="Filter"
            aria-label="Filter tasks"
            onInput={(event) => setFilter(event.currentTarget.value)}
          />
        </label>
      </header>

      <div class={composer}>
        <input
          class={composerInput}
          value={draft()}
          placeholder="New task"
          aria-label="New task"
          onInput={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') commit()
          }}
        />
        <span class={countBadge}>↵</span>
      </div>

      <div class={scroll}>
        <Show
          when={!isEmpty()}
          fallback={
            <div class={empty}>
              <span>Nothing here yet.</span>
              <span class={css({ fontSize: 'xs' })}>Type above to add the first task.</span>
            </div>
          }
        >
          <For each={sections()}>
            {(section) => (
              <div class={sectionGroup}>
                <div class={sectionHead}>
                  <span
                    class={cx(
                      sectionTitle,
                      section.id === 'overdue' ? overdueTone : section.id === 'today' ? todayTone : undefined,
                    )}
                  >
                    {section.title}
                  </span>
                  <span class={countBadge}>{section.tasks.length}</span>
                </div>
                <For each={section.tasks}>{(task) => <TaskRow task={task} />}</For>
              </div>
            )}
          </For>

          <Show when={completed().length > 0}>
            <div class={sectionGroup}>
              <div class={sectionHead}>
                <button
                  type="button"
                  class={toggle}
                  aria-expanded={showCompleted()}
                  onClick={() => setShowCompleted((open) => !open)}
                >
                  {showCompleted() ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
                  <span>Completed</span>
                </button>
                <span class={countBadge}>{completed().length}</span>
              </div>
              <Show when={showCompleted()}>
                <For each={completed()}>{(task) => <TaskRow task={task} />}</For>
              </Show>
            </div>
          </Show>
        </Show>
      </div>
    </section>
  )
}
