import { Match, Show, Switch, createEffect, createResource, createSignal, createUniqueId, onCleanup } from 'solid-js'

import { projectRemote, type ProjectRemote, type ProjectSnapshot } from './bridge'

interface ProjectRowProps {
  project: ProjectSnapshot
  homeDirectory?: string
  selected: boolean
  expanded: boolean
  pending: boolean
  showArchived: boolean
  archiveDisabled: boolean
  menuOpen: boolean
  onSelect: () => void
  onCompose: () => void
  onMenuOpenChange: (open: boolean) => void
  onToggleArchived: () => void
  onProjectSettings: () => void
}

export function ProjectRow(props: ProjectRowProps) {
  const [menuPosition, setMenuPosition] = createSignal({ left: 0, top: 0 })
  const pathId = createUniqueId()
  const [remote] = createResource(
    () => props.menuOpen && props.project.isGitRepository && props.project.id,
    async (projectId) => {
      try {
        return { value: await projectRemote(projectId), failed: false }
      } catch {
        return { value: null, failed: true }
      }
    },
  )
  let row: HTMLDivElement | undefined
  let menu: HTMLDivElement | undefined
  let projectButton: HTMLButtonElement | undefined
  let archiveButton: HTMLButtonElement | undefined
  let settingsButton: HTMLButtonElement | undefined

  function displayPath() {
    const path = props.project.path
    const home = props.homeDirectory?.replace(/[\\/]+$/, '')
    if (!home) return path
    if (path === home) return '~/'
    if (path.startsWith(`${home}/`) || path.startsWith(`${home}\\`)) return `~/${path.slice(home.length + 1)}`
    return path
  }

  function positionMenu() {
    const bounds = row?.getBoundingClientRect()
    if (!bounds) return
    const height = menu?.offsetHeight ?? 0
    setMenuPosition({ left: bounds.right, top: Math.min(bounds.top, window.innerHeight - height) })
  }

  function openMenu(focus: boolean) {
    positionMenu()
    props.onMenuOpenChange(true)
    if (focus) queueMicrotask(() => { if (props.menuOpen) archiveButton?.focus() })
  }

  function closeMenu(restoreFocus = false) {
    props.onMenuOpenChange(false)
    if (restoreFocus) projectButton?.focus({ preventScroll: true })
  }

  createEffect(() => {
    if (!props.menuOpen) return
    positionMenu()
    const list = row?.closest('.projects')
    const dismiss = () => closeMenu(menu?.contains(document.activeElement))
    const observer = new ResizeObserver(positionMenu)
    if (menu) observer.observe(menu)
    list?.addEventListener('scroll', dismiss)
    window.addEventListener('resize', positionMenu)
    onCleanup(() => {
      list?.removeEventListener('scroll', dismiss)
      window.removeEventListener('resize', positionMenu)
      observer.disconnect()
    })
  })

  return (
    <div
      ref={row}
      class="project-row"
      classList={{ 'project-row--selected': props.selected }}
      onPointerEnter={(event) => { if (event.pointerType === 'mouse') openMenu(false) }}
      onPointerLeave={() => { if (!menu?.contains(document.activeElement)) closeMenu() }}
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
        closeMenu(menu?.contains(document.activeElement))
      }}
    >
      <button
        ref={projectButton}
        type="button"
        class="project nav-item"
        aria-expanded={props.expanded}
        aria-current={props.selected ? 'true' : undefined}
        aria-keyshortcuts="Shift+F10"
        onKeyDown={(event) => {
          if (event.key !== 'ContextMenu' && !(event.shiftKey && event.key === 'F10')) return
          event.preventDefault()
          openMenu(true)
        }}
        onClick={() => {
          closeMenu()
          props.onSelect()
        }}
      >
        <span class="project-icon" aria-hidden="true">
          <FolderIcon size={16} />
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
        <div
          ref={menu}
          class="project-menu"
          style={{ '--project-menu-left': `${menuPosition().left}px`, top: `${menuPosition().top}px` }}
          role="menu"
          aria-label={props.project.name}
          aria-describedby={pathId}
          onKeyDown={(event) => {
            if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
            event.preventDefault()
            const next = document.activeElement === archiveButton ? settingsButton : archiveButton
            next?.focus()
          }}
        >
          <div class="project-menu__details" role="presentation">
            <p id={pathId} class="project-menu__location">
              <FolderIcon size={12} />
              <span title={props.project.path}>{displayPath()}</span>
            </p>
            <Show when={props.project.isGitRepository}>
              <Show
                when={remote()?.value}
                fallback={<p>{remote.loading ? 'Reading remote…' : remote()?.failed ? 'Could not read Git remote.' : 'No Git remote configured.'}</p>}
              >
                {(value) => (
                  <p class="project-menu__remote">
                    <Show when={value().forge}>
                      {(forge) => <ForgeIcon forge={forge()} />}
                    </Show>
                    <span title={value().label}>{value().label}</span>
                  </p>
                )}
              </Show>
            </Show>
          </div>
          <button
            ref={archiveButton}
            type="button"
            role="menuitemcheckbox"
            class="nav-item"
            aria-checked={props.showArchived}
            aria-disabled={props.archiveDisabled}
            tabindex={-1}
            onClick={() => { if (!props.archiveDisabled) props.onToggleArchived() }}
          >
            Show archived
            <span class="project-menu__switch" aria-hidden="true" />
          </button>
          <button
            ref={settingsButton}
            type="button"
            role="menuitem"
            class="nav-item"
            tabindex={-1}
            onClick={() => {
              closeMenu(document.activeElement === settingsButton)
              props.onProjectSettings()
            }}
          >
            <svg class="project-menu__icon" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true">
              <path d="m6 3 .5-1.5h3L10 3l1 .6 1.6-.3 1.5 2.6-1.1 1.2v1.8l1.1 1.2-1.5 2.6-1.6-.3-1 .6-.5 1.5h-3L6 13l-1-.6-1.6.3-1.5-2.6L3 8.9V7.1L1.9 5.9l1.5-2.6L5 3.6Z" />
              <circle cx="8" cy="8" r="2.1" />
            </svg>
            Project settings
          </button>
        </div>
      </Show>
    </div>
  )
}

function FolderIcon(props: { size: number }) {
  return (
    <svg width={props.size} height={props.size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </svg>
  )
}

const forgeNames = { github: 'GitHub', gitlab: 'GitLab', huggingface: 'Hugging Face' }

function ForgeIcon(props: { forge: NonNullable<ProjectRemote['forge']> }) {
  return (
    <svg class="project-menu__icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" role="img" aria-label={forgeNames[props.forge]}>
      <Switch>
        <Match when={props.forge === 'github'}>
          <path d="M9 19c-5 1.5-5-2.5-7-3m14 6v-3.87a3.37 3.37 0 0 0-.94-2.61c3.14-.35 6.44-1.54 6.44-7A5.44 5.44 0 0 0 20 4.77 5.07 5.07 0 0 0 19.91 1S18.73.65 16 2.48a13.38 13.38 0 0 0-7 0C6.27.65 5.09 1 5.09 1A5.07 5.07 0 0 0 5 4.77a5.44 5.44 0 0 0-1.5 3.78c0 5.42 3.3 6.61 6.44 7A3.37 3.37 0 0 0 9 18.13V22" />
        </Match>
        <Match when={props.forge === 'gitlab'}>
          <path d="M22.65 14.39L12 22.13 1.35 14.39a.84.84 0 0 1-.3-.94l1.22-3.78 2.44-7.51A.42.42 0 0 1 4.82 2a.43.43 0 0 1 .58 0 .42.42 0 0 1 .11.18l2.44 7.49h8.1l2.44-7.51A.42.42 0 0 1 18.6 2a.43.43 0 0 1 .58 0 .42.42 0 0 1 .11.18l2.44 7.51L23 13.45a.84.84 0 0 1-.35.94z" />
        </Match>
        <Match when={props.forge === 'huggingface'}>
          <path d="M4 12a8 8 0 1 1 16 0M9 10h.01M15 10h.01M9.5 13a3.2 3.2 0 0 0 5 0M10 20a5 5 0 0 1-6-1l-2-3a1.5 1.5 0 0 1 2.5-1.5l2 2-1-3m8.5 6.5a5 5 0 0 0 6-1l2-3a1.5 1.5 0 0 0-2.5-1.5l-2 2 1-3M10 20a8 8 0 0 0 4 0" />
        </Match>
      </Switch>
    </svg>
  )
}
