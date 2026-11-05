import { For, Show, createEffect, createMemo, createSignal, createUniqueId } from 'solid-js'

import type { ProjectNode, ProjectScan } from './bridge'

const upcomingImportSources = ['OpenCode', 'Pi', 'Claude Code', 'Freebuff']

interface ProjectImportSourcesProps {
  codexDisabled: boolean
  note: string | null
  onImportCodex: () => void
}

export function ProjectImportSources(props: ProjectImportSourcesProps) {
  return (
    <>
      <div class="projects__sources" role="group" aria-label="Project import sources">
        <button type="button" class="project-action" onClick={props.onImportCodex} disabled={props.codexDisabled}>
          ChatGPT app
        </button>
        <For each={upcomingImportSources}>
          {(source) => <button type="button" class="project-action" disabled>{source}</button>}
        </For>
      </div>
      <Show when={props.note}>
        {(note) => <p class="empty" role="status">{note()}</p>}
      </Show>
    </>
  )
}

interface ProjectPickerProps {
  scan: ProjectScan | null
  loading: boolean
  error: string | null
  onRescan: () => void
  onCancel: () => void
  onSelect: (path: string) => void
}

interface PickerRow {
  node: ProjectNode
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

  const roots = createMemo(() => {
    let nodes = props.scan?.tree ?? []
    while (nodes.length === 1) {
      const [only] = nodes
      if (only.repository || only.children.length === 0) break
      nodes = only.children
    }
    return nodes
  })
  const needle = () => filter().trim().toLowerCase()
  const visible = createMemo(() => {
    const rows: PickerRow[] = []
    const query = needle()
    const open = expansion()
    function visit(nodes: ProjectNode[], depth: number, parent?: string) {
      for (const node of nodes) {
        if (query) {
          if (node.repository && (node.name.toLowerCase().includes(query) || node.path.toLowerCase().includes(query))) {
            rows.push({ node, depth: 0 })
          } else {
            visit(node.children, 0)
          }
        } else if (node.repository) {
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
    if (row.node.repository) {
      props.onSelect(row.node.path)
      return
    }
    setExpansion((previous) => new Map(previous).set(row.node.path, !row.expanded))
    input?.focus({ preventScroll: true })
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
          <button type="button" onClick={props.onRescan} disabled={props.loading}>Re-scan</button>
          <button type="button" aria-label="Close picker" onClick={props.onCancel}>Close</button>
        </header>

        <p class="picker__root">{props.scan?.root ?? 'Home'}</p>
        <input
          class="picker__filter"
          ref={input}
          role="combobox"
          aria-label="Filter repositories"
          aria-controls={visible().length ? treeId : undefined}
          aria-activedescendant={visible().length ? rowId(activeIndex()) : undefined}
          aria-expanded={visible().length > 0}
          aria-haspopup="tree"
          aria-autocomplete="list"
          placeholder="Filter repositories"
          autofocus
          value={filter()}
          onInput={(event) => {
            setFilter(event.currentTarget.value)
            setActivePath(undefined)
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
            <ul class="picker__list" role="tree" aria-label="Repositories" id={treeId}>
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
                      <span class="picker__kind">{row.node.repository ? 'repository' : 'folder'}</span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>

        <footer class="picker__hint">↑ ↓ Move · ← → Collapse or expand · Enter Open · Esc Cancel</footer>
      </div>
    </div>
  )
}
