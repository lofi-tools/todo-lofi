import { Show, createSignal } from 'solid-js'

import type { ProjectSnapshot } from './bridge'

interface ProjectRowProps {
  project: ProjectSnapshot
  selected: boolean
  pending: boolean
  showArchived: boolean
  menuOpen: boolean
  onSelect: () => void
  onCompose: () => void
  onMenuOpenChange: (open: boolean) => void
  onToggleArchived: () => void
}

export function ProjectRow(props: ProjectRowProps) {
  const [menuAbove, setMenuAbove] = createSignal(false)
  let row: HTMLDivElement | undefined
  let menuButton: HTMLButtonElement | undefined
  let archiveButton: HTMLButtonElement | undefined

  function openMenu(focus: boolean) {
    const bounds = row?.getBoundingClientRect()
    const list = row?.closest('.projects')?.getBoundingClientRect()
    setMenuAbove(!!(bounds && list && list.bottom - bounds.bottom < 44 && bounds.top - list.top >= 44))
    props.onMenuOpenChange(true)
    if (focus) queueMicrotask(() => { if (props.menuOpen) archiveButton?.focus() })
  }

  function closeMenu(restoreFocus = false) {
    props.onMenuOpenChange(false)
    if (restoreFocus) menuButton?.focus()
  }

  return (
    <div
      ref={row}
      class="project-row"
      classList={{ 'project-row--selected': props.selected }}
      onPointerEnter={(event) => { if (event.pointerType === 'mouse') openMenu(false) }}
      onPointerLeave={() => { if (document.activeElement !== archiveButton) closeMenu() }}
      onFocusOut={(event) => {
        if (!(event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget))) closeMenu()
      }}
      onContextMenu={(event) => {
        event.preventDefault()
        openMenu(true)
      }}
      onKeyDown={(event) => {
        if (event.key !== 'Escape' || !props.menuOpen) return
        event.preventDefault()
        closeMenu(document.activeElement === archiveButton)
      }}
    >
      <button
        type="button"
        class="project nav-item"
        aria-expanded={props.selected}
        aria-current={props.selected ? 'true' : undefined}
        onClick={() => {
          closeMenu()
          props.onSelect()
        }}
      >
        <span class="project-icon" aria-hidden="true">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
          </svg>
          <Show when={props.project.isGitRepository}>
            <svg class="project-icon__git" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
              <path d="M15 6a9 9 0 0 0-9 9V3" />
              <circle cx="18" cy="6" r="3" />
              <circle cx="6" cy="18" r="3" />
            </svg>
          </Show>
        </span>
        <span class="project__name">{props.project.name}</span>
      </button>
      <button
        ref={menuButton}
        type="button"
        class="project-row__action"
        aria-label={`Project menu for ${props.project.name}`}
        title="Project menu"
        aria-haspopup="menu"
        aria-expanded={props.menuOpen}
        onClick={() => openMenu(true)}
        onKeyDown={(event) => {
          if (event.key !== 'ArrowDown') return
          event.preventDefault()
          openMenu(true)
        }}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <circle cx="5" cy="12" r="2" />
          <circle cx="12" cy="12" r="2" />
          <circle cx="19" cy="12" r="2" />
        </svg>
      </button>
      <button
        type="button"
        class="project-row__action"
        aria-label={`New chat in ${props.project.name}`}
        title="New chat"
        disabled={props.pending}
        onClick={() => {
          closeMenu()
          props.onCompose()
        }}
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
          <path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z" />
        </svg>
      </button>
      <Show when={props.menuOpen}>
        <div class="project-menu" classList={{ 'project-menu--above': menuAbove() }} role="menu" aria-label={props.project.name}>
          <button
            ref={archiveButton}
            type="button"
            role="menuitemcheckbox"
            class="nav-item"
            aria-checked={props.showArchived}
            tabindex={-1}
            onClick={() => {
              props.onToggleArchived()
              closeMenu(document.activeElement === archiveButton)
            }}
          >
            <span class="project-menu__check" aria-hidden="true">{props.showArchived ? '✓' : ''}</span>
            Show archived
          </button>
        </div>
      </Show>
    </div>
  )
}
