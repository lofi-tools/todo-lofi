/**
 * The project picker: a drill-down over the cached scan, one level at a time,
 * with a filter that searches the whole tree. Selecting a repository adds it;
 * selecting a directory opens its children.
 */

import { For, Show, createMemo, createSignal } from 'solid-js'

import * as bridge from './bridge'

interface ProjectPickerProps {
  scan: bridge.ProjectScan | null
  loading: boolean
  error: string | null
  onRescan: () => void
  onCancel: () => void
  onSelect: (path: string) => void
}

function flatten(nodes: bridge.ProjectNode[]): bridge.ProjectNode[] {
  return nodes.flatMap((node) => [node, ...flatten(node.children)])
}

export function ProjectPicker(props: ProjectPickerProps) {
  const [trail, setTrail] = createSignal<bridge.ProjectNode[]>([])
  const [filter, setFilter] = createSignal('')
  const [cursor, setCursor] = createSignal(0)

  const level = () => {
    const stack = trail()
    const current = stack[stack.length - 1]
    return current ? current.children : (props.scan?.tree ?? [])
  }

  const needle = () => filter().trim().toLowerCase()

  const visible = createMemo(() => {
    const query = needle()
    if (!query) return level()
    return flatten(props.scan?.tree ?? []).filter(
      (node) =>
        node.name.toLowerCase().includes(query) || node.path.toLowerCase().includes(query),
    )
  })

  const activeIndex = () => Math.min(cursor(), Math.max(visible().length - 1, 0))

  function reset() {
    setFilter('')
    setCursor(0)
  }

  function open(node: bridge.ProjectNode) {
    if (node.repository) {
      props.onSelect(node.path)
      return
    }
    setTrail([...trail(), node])
    reset()
  }

  function move(delta: number) {
    const count = visible().length
    if (count === 0) return
    setCursor((activeIndex() + delta + count) % count)
  }

  function choose() {
    const node = visible()[activeIndex()]
    if (node) open(node)
  }

  function handleKeyDown(event: KeyboardEvent) {
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault()
        move(1)
        break
      case 'ArrowUp':
        event.preventDefault()
        move(-1)
        break
      case 'Enter':
        event.preventDefault()
        choose()
        break
      case 'Escape':
        event.preventDefault()
        props.onCancel()
        break
      case 'Backspace':
        if (filter() === '' && trail().length > 0) {
          event.preventDefault()
          setTrail(trail().slice(0, -1))
          setCursor(0)
        }
        break
    }
  }

  return (
    <div class="picker-backdrop">
      <div
        class="picker"
        role="dialog"
        aria-modal="true"
        aria-label="Add project"
        onKeyDown={handleKeyDown}
      >
        <header class="picker__header">
          <h3>Add project</h3>
          <button type="button" onClick={props.onRescan} disabled={props.loading}>
            Re-scan
          </button>
          <button type="button" aria-label="Close picker" onClick={props.onCancel}>
            Close
          </button>
        </header>

        <nav class="picker__trail" aria-label="Directories">
          <button
            type="button"
            class="picker__crumb"
            onClick={() => {
              setTrail([])
              reset()
            }}
          >
            {props.scan?.root ?? 'Home'}
          </button>
          <For each={trail()}>
            {(node, index) => (
              <button
                type="button"
                class="picker__crumb"
                onClick={() => {
                  setTrail(trail().slice(0, index() + 1))
                  reset()
                }}
              >
                {node.name}
              </button>
            )}
          </For>
        </nav>

        <input
          class="picker__filter"
          aria-label="Filter repositories"
          placeholder="Filter repositories"
          autofocus
          value={filter()}
          onInput={(event) => {
            setFilter(event.currentTarget.value)
            setCursor(0)
          }}
        />

        <Show when={props.error}>
          {(message) => <p class="empty">{message()}</p>}
        </Show>

        <Show when={!props.error}>
          <Show
            when={visible().length > 0}
            fallback={
              <p class="empty">
                {props.loading
                  ? 'Scanning for Git repositories…'
                  : needle()
                    ? 'No repositories match the filter.'
                    : 'No Git repositories here.'}
              </p>
            }
          >
            <ul class="picker__list" role="listbox" aria-label="Directories">
              <For each={visible()}>
                {(node, index) => (
                  <li role="presentation">
                    <button
                      type="button"
                      role="option"
                      class="picker__row"
                      classList={{ 'picker__row--active': index() === activeIndex() }}
                      aria-selected={index() === activeIndex()}
                      onClick={() => open(node)}
                    >
                      <span class="picker__name">{node.name}</span>
                      <span class="picker__kind">
                        {node.repository
                          ? 'repository'
                          : `${node.children.length} ${node.children.length === 1 ? 'entry' : 'entries'}`}
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>

        <footer class="picker__hint">Arrow keys move · Enter selects · Esc cancels</footer>
      </div>
    </div>
  )
}
