import { Match, Show, Switch, createResource, createSignal, createUniqueId } from 'solid-js'

import { projectRemote, type ProjectRemote, type ProjectSnapshot } from './bridge'
import { focusSidebarMenu, isSidebarMenuKey, SidebarMenu, type SidebarMenuPoint } from './SidebarMenu'

interface ProjectRowProps {
  project: ProjectSnapshot
  homeDirectory?: string
  selected: boolean
  containsActiveChat: boolean
  expanded: boolean
  pending: boolean
  showArchived: boolean
  archiveDisabled: boolean
  menuOpen: boolean
  processCount: number
  reorderDisabled: boolean
  onPointerDownProject: (event: PointerEvent) => void
  onMoveProject: (direction: number) => void
  onProcesses: () => void
  onSelect: () => void
  onCompose: () => void
  onMenuOpenChange: (open: boolean) => void
  onToggleArchived: () => void
  onProjectSettings: () => void
  copyDetails: () => Promise<string>
}

export function ProjectRow(props: ProjectRowProps) {
  const [menuCursor, setMenuCursor] = createSignal<SidebarMenuPoint>()
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
  let row!: HTMLDivElement
  let projectButton: HTMLButtonElement | undefined

  function displayPath() {
    const path = props.project.path
    const home = props.homeDirectory?.replace(/[\\/]+$/, '')
    if (!home) return path
    if (path === home) return '~/'
    if (path.startsWith(`${home}/`) || path.startsWith(`${home}\\`)) return `~/${path.slice(home.length + 1)}`
    return path
  }

  function openMenu(focus: boolean, cursor?: SidebarMenuPoint) {
    setMenuCursor(cursor)
    props.onMenuOpenChange(true)
    if (focus) focusSidebarMenu(row)
  }

  function closeMenu(restoreFocus = false) {
    props.onMenuOpenChange(false)
    if (restoreFocus) projectButton?.focus({ preventScroll: true })
  }

  return (
    <div
      ref={row}
      class="project-row"
      classList={{ 'project-row--selected': props.selected, 'project-row--active-chat': props.containsActiveChat }}
      onPointerEnter={(event) => {
        if (event.pointerType === 'mouse' && !document.activeElement?.closest('.project-menu')) openMenu(false)
      }}
      onPointerLeave={() => { if (!row.querySelector('.project-menu')?.contains(document.activeElement)) closeMenu() }}
      onFocusOut={(event) => {
        if (!(event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget))) closeMenu()
      }}
      on:contextmenu={(event) => {
        event.preventDefault()
        openMenu(true, { x: event.clientX, y: event.clientY })
      }}
      onKeyDown={(event) => {
        if (event.key !== 'Escape' || !props.menuOpen) return
        event.preventDefault()
        closeMenu()
      }}
    >
      <button
        ref={projectButton}
        type="button"
        class="project nav-item"
        data-reorderable={!props.reorderDisabled}
        aria-expanded={props.expanded}
        aria-current={props.selected ? 'true' : undefined}
        aria-haspopup="menu"
        aria-keyshortcuts="Shift+F10 Alt+ArrowUp Alt+ArrowDown"
        onDragStart={(event) => event.preventDefault()}
        onPointerDown={(event) => { closeMenu(); props.onPointerDownProject(event) }}
        onKeyDown={(event) => {
          if (event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
            event.preventDefault()
            if (!props.reorderDisabled) props.onMoveProject(event.key === 'ArrowUp' ? -1 : 1)
            return
          }
          if (!isSidebarMenuKey(event)) return
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
        {/* A project with one conversation never shows task rows, so its own
            row carries the waiting signal. */}
        <Show when={props.project.waitingTaskCount === 1 && props.project.unfinishedTaskCount === 1}>
          <WaitingIcon />
        </Show>
      </button>
      <Show when={props.processCount > 0}>
        <button type="button" class="process-indicator" aria-label={`${props.processCount} running ${props.processCount === 1 ? 'process' : 'processes'} in ${props.project.name}`} title="Running processes" onClick={props.onProcesses}>{props.processCount}</button>
      </Show>
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
        <SidebarMenu row={row} cursor={menuCursor()} label={props.project.name} descriptionId={pathId}
          copyDetails={props.copyDetails} onClose={closeMenu}>
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
            type="button"
            role="menuitem"
            class="nav-item"
            tabindex={-1}
            onClick={(event) => {
              closeMenu(document.activeElement === event.currentTarget)
              props.onProjectSettings()
            }}
          >
            <svg class="project-menu__icon" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true">
              <path d="m6 3 .5-1.5h3L10 3l1 .6 1.6-.3 1.5 2.6-1.1 1.2v1.8l1.1 1.2-1.5 2.6-1.6-.3-1 .6-.5 1.5h-3L6 13l-1-.6-1.6.3-1.5-2.6L3 8.9V7.1L1.9 5.9l1.5-2.6L5 3.6Z" />
              <circle cx="8" cy="8" r="2.1" />
            </svg>
            Project settings
          </button>
        </SidebarMenu>
      </Show>
    </div>
  )
}

export function WaitingIcon() {
  return (
    <svg class="waiting-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" role="img" aria-label="Waiting for you">
      <path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z" />
      <path d="M9.7 9.4a2.4 2.4 0 1 1 3.4 2.2c-.6.3-1 .8-1 1.5v.3" />
      <path d="M12 16.6h.01" />
    </svg>
  )
}

export function FolderIcon(props: { size: number }) {
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
