import { For, Show, createMemo, createSignal, onCleanup, onMount } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { Search } from 'web-design-system/icons'
import { Dialog } from 'web-design-system/headless'
import { useApp, type Destination } from '../store'

interface Command {
  id: string
  label: string
  hint?: string
  run: () => void
}

const backdrop = css({
  position: 'fixed',
  inset: '0',
  bg: 'overlay',
  zIndex: 'overlay',
  animation: 'fadeIn 0.12s ease-out',
})

const positioner = css({
  position: 'fixed',
  inset: '0',
  zIndex: 'overlay',
  display: 'grid',
  placeItems: 'start center',
  pt: { base: '12', md: '20' },
  px: '4',
  pointerEvents: 'none',
})

const content = css({
  w: 'full',
  maxW: '560px',
  pointerEvents: 'auto',
  bg: 'surface.elevated',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border',
  borderRadius: 'lg',
  boxShadow: 'lg',
  overflow: 'hidden',
  animation: 'slideUp 0.15s ease-out',
})

const field = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  px: '3',
  h: '11',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
  color: 'fg.subtle',
})

const input = css({
  flex: '1',
  minW: '0',
  bg: 'transparent',
  borderWidth: '0',
  color: 'fg.default',
  fontSize: 'sm',
  _focusVisible: { outline: 'none' },
  _placeholder: { color: 'fg.subtle' },
})

const list = css({ display: 'flex', flexDirection: 'column', maxH: '80', overflowY: 'auto', p: '1' })

const item = css({
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: '3',
  px: '2',
  h: '9',
  borderRadius: 'sm',
  fontSize: 'sm',
  color: 'fg.default',
  cursor: 'pointer',
  _hover: { bg: 'surface.hover' },
})

const hint = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const empty = css({ px: '3', py: '6', textAlign: 'center', fontSize: 'xs', color: 'fg.subtle' })

/** The command palette: jump to a destination, open a task, or start a new one. */
export default function CommandPalette() {
  const app = useApp()
  const [query, setQuery] = createSignal('')

  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== 'k' || !(event.metaKey || event.ctrlKey)) return
      event.preventDefault()
      app.toggleCommand()
    }
    window.addEventListener('keydown', onKeyDown)
    onCleanup(() => window.removeEventListener('keydown', onKeyDown))
  })

  const destinations: { label: string; destination: Destination }[] = [
    { label: 'Go to All tasks', destination: { kind: 'all' } },
    { label: 'Go to Apps', destination: { kind: 'apps' } },
    { label: 'Go to Integrations', destination: { kind: 'integrations' } },
    { label: 'Go to Automations', destination: { kind: 'automations' } },
    { label: 'Go to Sync history', destination: { kind: 'sync' } },
    { label: 'Go to Settings', destination: { kind: 'settings' } },
  ]

  const commands = createMemo<Command[]>(() => {
    const list: Command[] = destinations.map((entry, index) => ({
      id: `dest-${index}`,
      label: entry.label,
      hint: 'Navigate',
      run: () => app.setDestination(entry.destination),
    }))

    for (const tag of app.state.tags) {
      list.push({
        id: `tag-${tag.id}`,
        label: `Go to #${tag.name}`,
        hint: 'Tag',
        run: () => app.setDestination({ kind: 'tag', tagId: tag.id }),
      })
    }

    for (const task of app.state.tasks.filter((entry) => !entry.completed).slice(0, 20)) {
      list.push({
        id: `task-${task.id}`,
        label: task.title,
        hint: 'Open task',
        run: () => app.selectTask(task.id),
      })
    }

    return list
  })

  const filtered = createMemo(() => {
    const needle = query().trim().toLowerCase()
    if (!needle) return commands().slice(0, 12)
    return commands()
      .filter((command) => command.label.toLowerCase().includes(needle))
      .slice(0, 12)
  })

  const run = (command: Command) => {
    command.run()
    app.toggleCommand()
  }

  return (
    <Dialog.Root
      open={app.state.commandOpen}
      onOpenChange={(details) => {
        if (!details.open) {
          setQuery('')
          app.toggleCommand()
        }
      }}
    >
      <Dialog.Backdrop class={backdrop} />
      <Dialog.Positioner class={positioner}>
        <Dialog.Content class={content}>
          <Dialog.Title class={css({ srOnly: true })}>Command palette</Dialog.Title>
          <div class={field}>
            <Search size={14} />
            <input
              class={input}
              value={query()}
              autofocus
              placeholder="Search tasks, tags, and views…"
              aria-label="Search commands"
              onInput={(event) => setQuery(event.currentTarget.value)}
            />
            <kbd class={hint}>Esc</kbd>
          </div>

          <Show
            when={filtered().length > 0}
            fallback={<p class={empty}>No matches for “{query()}”.</p>}
          >
            <div class={list}>
              <For each={filtered()}>
                {(command) => (
                  <button type="button" class={cx(item)} onClick={() => run(command)}>
                    <span class={css({ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' })}>
                      {command.label}
                    </span>
                    <span class={hint}>{command.hint}</span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </Dialog.Content>
      </Dialog.Positioner>
    </Dialog.Root>
  )
}
