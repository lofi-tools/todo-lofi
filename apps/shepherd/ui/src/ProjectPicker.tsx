import { For, Show, createEffect, createMemo, createSignal, createUniqueId } from 'solid-js'

import type { LocalCodexProject, ProjectNode, ProjectScan } from './bridge'

interface ProjectPickerProps {
  scan: ProjectScan | null
  desktopProjects?: LocalCodexProject[]
  loading: boolean
  error: string | null
  onSwitchSource: () => void
  onRescan: () => void
  onCancel: () => void
  onSelect: (paths: string[]) => void
}

type PickerNode = ProjectNode | LocalCodexProject

interface PickerRow {
  node: PickerNode
  depth: number
  parent?: string
  expanded?: boolean
}

export function ProjectPicker(props: ProjectPickerProps) {
  const [expansion, setExpansion] = createSignal(new Map<string, boolean>())
  const [filter, setFilter] = createSignal('')
  const [activePath, setActivePath] = createSignal<string>()
  const treeId = createUniqueId()
  let input: HTMLInputElement | undefined

  const fromChatGpt = () => props.desktopProjects !== undefined
  const filterLabel = () => fromChatGpt() ? 'Filter ChatGPT projects' : 'Filter repositories'
  const roots = createMemo<PickerNode[]>(() => {
    if (props.desktopProjects) return props.desktopProjects
    let nodes = props.scan?.tree ?? []
    while (nodes.length === 1) {
      const [only] = nodes
      if (only.repository || only.children.length === 0) break
      nodes = only.children
    }
    return nodes
  })
  const needle = () => filter().trim().toLowerCase()
  const emptyMessage = () => {
    if (fromChatGpt()) {
      if (props.loading) return 'Loading ChatGPT projects…'
      return needle() ? 'No projects match the filter.' : 'No new local folders to import.'
    }
    if (props.loading) return 'Scanning for Git repositories…'
    return needle() ? 'No repositories match the filter.' : 'No Git repositories here.'
  }
  const visible = createMemo(() => {
    const rows: PickerRow[] = []
    const query = needle()
    const open = expansion()
    function visit(nodes: PickerNode[], depth: number, parent?: string) {
      for (const node of nodes) {
        const group = 'children' in node && !node.repository
        if (query) {
          if (!group && (node.name.toLowerCase().includes(query) || node.path.toLowerCase().includes(query))) {
            rows.push({ node, depth: 0 })
          } else if (group) {
            visit(node.children, 0)
          }
        } else if (!group) {
          rows.push({ node, depth, parent })
        } else {
          const expanded = open.get(node.path) ?? depth === 0
          rows.push({ node, depth, parent, expanded })
          if (expanded) visit(node.children, depth + 1, node.path)
        }
      }
    }
    visit(roots(), 0)
    return rows
  })

  const activeIndex = () => Math.max(0, visible().findIndex(({ node }) => node.path === activePath()))
  const rowId = (index: number) => `${treeId}-${index}`

  createEffect(() => {
    if (!activePath()) return
    document.getElementById(rowId(activeIndex()))?.scrollIntoView({ block: 'nearest' })
  })

  function toggle(row: PickerRow) {
    setActivePath(row.node.path)
    if (row.expanded === undefined) {
      props.onSelect([row.node.path])
      return
    }
    setExpansion((previous) => new Map(previous).set(row.node.path, !row.expanded))
    input?.focus({ preventScroll: true })
  }

  function switchSource() {
    setFilter('')
    setActivePath(undefined)
    props.onSwitchSource()
    input?.focus()
  }

  function handleKeyDown(event: KeyboardEvent) {
    if (event.isComposing) return
    if (event.key === 'Escape') {
      event.preventDefault()
      props.onCancel()
      return
    }
    if (event.target !== input) return
    const rows = visible()
    const index = activeIndex()
    const row = rows[index]
    if (!row) return
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowUp': {
        event.preventDefault()
        const next = (index + (event.key === 'ArrowDown' ? 1 : -1) + rows.length) % rows.length
        setActivePath(rows[next].node.path)
        break
      }
      case 'ArrowRight':
        if (row.expanded === undefined) break
        event.preventDefault()
        if (!row.expanded) toggle(row)
        else if (rows[index + 1]?.parent === row.node.path) setActivePath(rows[index + 1].node.path)
        break
      case 'ArrowLeft':
        if (!row.expanded && !row.parent) break
        event.preventDefault()
        if (row.expanded) toggle(row)
        else setActivePath(row.parent)
        break
      case 'Enter':
        event.preventDefault()
        toggle(row)
        break
    }
  }

  return (
    <div class="picker-backdrop">
      <div class="picker" role="dialog" aria-modal="true" aria-label="Add project" onKeyDown={handleKeyDown}>
        <header class="picker__header">
          <h3>Add project</h3>
          <button type="button" onClick={switchSource}>
            {fromChatGpt() ? 'Browse repositories' : 'From ChatGPT app'}
          </button>
          <button type="button" onClick={props.onRescan} disabled={props.loading}>
            {fromChatGpt() ? 'Refresh' : 'Re-scan'}
          </button>
          <Show when={props.desktopProjects?.length}>
            <button type="button" onClick={() => props.onSelect(props.desktopProjects?.map((project) => project.path) ?? [])}>
              Import all
            </button>
          </Show>
          <button type="button" aria-label="Close picker" onClick={props.onCancel}>Close</button>
        </header>

        <p class="picker__root">{fromChatGpt() ? 'Primary folders from local Codex projects' : props.scan?.root ?? 'Home'}</p>
        <input
          class="picker__filter"
          ref={input}
          role="combobox"
          aria-label={filterLabel()}
          aria-controls={visible().length ? treeId : undefined}
          aria-activedescendant={visible().length ? rowId(activeIndex()) : undefined}
          aria-expanded={visible().length > 0}
          aria-haspopup="tree"
          aria-autocomplete="list"
          placeholder={filterLabel()}
          autofocus
          value={filter()}
          onInput={(event) => {
            setFilter(event.currentTarget.value)
            setActivePath(undefined)
          }}
        />

        <Show when={props.error}>
          {(message) => <p class="empty" role="alert">{message()}</p>}
        </Show>

        <Show when={!props.error}>
          <Show
            when={visible().length > 0}
            fallback={<p class="empty">{emptyMessage()}</p>}
          >
            <ul class="picker__list" role="tree" aria-label={fromChatGpt() ? 'ChatGPT app projects' : 'Repositories'} id={treeId}>
              <For each={visible()}>
                {(row, index) => (
                  <li role="presentation">
                    <button
                      id={rowId(index())}
                      type="button"
                      role="treeitem"
                      tabindex={-1}
                      class="picker__row"
                      classList={{ 'picker__row--active': index() === activeIndex() }}
                      style={{ 'padding-inline-start': `${8 + row.depth * 16}px` }}
                      aria-level={row.depth + 1}
                      aria-expanded={row.expanded}
                      aria-selected={index() === activeIndex()}
                      title={row.node.path}
                      onClick={() => toggle(row)}
                    >
                      <span class="picker__disclosure" aria-hidden="true">
                        {row.expanded === undefined ? '' : row.expanded ? '▾' : '▸'}
                      </span>
                      <span class="picker__name">{row.node.name}</span>
                      <span class="picker__kind" classList={{ 'picker__path': fromChatGpt() }}>
                        {fromChatGpt() ? row.node.path : row.expanded === undefined ? 'repository' : 'folder'}
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>

        <footer class="picker__hint">
          {fromChatGpt() ? '↑ ↓ Move · Enter Import · Esc Cancel' : '↑ ↓ Move · ← → Collapse or expand · Enter Open · Esc Cancel'}
        </footer>
      </div>
    </div>
  )
}
