import { For, Show, createSignal, onCleanup, onMount } from 'solid-js'

import type { ProjectSnapshot, TaskSummary } from './bridge'
import { WaitingIcon } from './ProjectRow'
import { focusSidebarMenu, isSidebarMenuKey, isSidebarSelection, sidebarDetails, SidebarMenu, type SidebarMenuPoint } from './SidebarMenu'

export function TaskRows(props: {
  tasks: TaskSummary[]
  project: ProjectSnapshot
  isMenuOpen: (taskId: string) => boolean
  onMenuOpenChange: (taskId: string, open: boolean) => void
  currentTaskId: string | null
  selection: () => Set<string> | undefined
  disabled?: boolean
  mac: boolean
  needsTerminalAttention: (taskId: string) => boolean
  processCount: (taskId: string) => number
  onProcesses: (taskId: string) => void
  isOutputUnread: (taskId: string) => boolean
  onOpen: (task: TaskSummary) => void
  onSelect: (taskId: string, extend: boolean) => void
}) {
  return (
    <div class="tasks">
      <ul class="task-list">
        <For each={props.tasks}>
          {(task) => {
            const [menuCursor, setMenuCursor] = createSignal<SidebarMenuPoint>()
            let row!: HTMLLIElement
            let button!: HTMLButtonElement
            const openMenu = (event: Event, cursor?: SidebarMenuPoint) => {
              event.preventDefault()
              setMenuCursor(cursor)
              props.onMenuOpenChange(task.id, true)
              focusSidebarMenu(row)
            }
            const closeMenu = (restoreFocus: boolean) => {
              props.onMenuOpenChange(task.id, false)
              if (restoreFocus) button.focus({ preventScroll: true })
            }
            return (
              <li ref={row} class="task-list__row"
                on:contextmenu={(event) => openMenu(event, { x: event.clientX, y: event.clientY })}
                onFocusOut={(event) => {
                  if (!(event.relatedTarget instanceof Node && row.contains(event.relatedTarget))) closeMenu(false)
                }}>
                <Show when={props.selection()}>
                  {(selection) => (
                    <input type="checkbox" aria-label={`Select ${task.title}`} disabled={props.disabled}
                      checked={selection().has(task.id)} onClick={(event) => props.onSelect(task.id, event.shiftKey)} />
                  )}
                </Show>
                <button ref={button} type="button" class={`task task--${task.state}`}
                  aria-current={task.id === props.currentTaskId ? 'true' : undefined}
                  aria-haspopup="menu"
                  aria-keyshortcuts="Shift+F10"
                  onKeyDown={(event) => { if (isSidebarMenuKey(event)) openMenu(event) }}
                  onClick={(event) => {
                    if (!isSidebarSelection(event, props.mac)) return
                    closeMenu(false)
                    if (props.selection() && (event.shiftKey || (props.mac ? event.metaKey : event.ctrlKey))) {
                      if (!props.disabled) props.onSelect(task.id, event.shiftKey)
                    } else {
                      props.onOpen(task)
                    }
                  }}>
                  <span class="task__state" aria-hidden="true" />
                  <span class="task__title">{task.title}</span>
                  <Show when={props.needsTerminalAttention(task.id)}>
                    <span class="terminal-attention" role="img" aria-label="Terminal needs attention" />
                  </Show>
                  <Show when={props.isOutputUnread(task.id)}>
                    <span class="task__unread" role="img" aria-label="Unread output" />
                  </Show>
                  <Show when={task.state === 'review' || task.state === 'waiting'}>
                    <Show when={task.state === 'review'} fallback={<WaitingIcon />}>
                      <span class="task__attention-badge">Needs review</span>
                    </Show>
                  </Show>
                </button>
                <Show when={props.processCount(task.id) > 0}>
                  <button type="button" class="process-indicator" aria-label={`${props.processCount(task.id)} running ${props.processCount(task.id) === 1 ? 'process' : 'processes'} in ${task.title}`} title="Running processes" onClick={() => props.onProcesses(task.id)}>{props.processCount(task.id)}</button>
                </Show>
                <Show when={props.isMenuOpen(task.id)}>
                  <SidebarMenu row={row} cursor={menuCursor()} label={task.title} onClose={closeMenu}
                    copyDetails={() => sidebarDetails(props.project, task, 'task')} />
                </Show>
              </li>
            )
          }}
        </For>
      </ul>
    </div>
  )
}

export function ReviewArchiveDialog(props: { count: number; onCancel: () => void; onConfirm: () => void }) {
  let dialog: HTMLDialogElement | undefined
  let cancel: HTMLButtonElement | undefined
  onMount(() => {
    dialog?.showModal()
    cancel?.focus()
    onCleanup(() => dialog?.close())
  })
  return (
    <dialog ref={dialog} class="picker picker-dialog" aria-label="Archive selected tasks"
      onCancel={(event) => { event.preventDefault(); props.onCancel() }}>
      <header class="picker__header">
        <h3>Archive {props.count} selected {props.count === 1 ? 'task' : 'tasks'}?</h3>
        <button type="button" ref={cancel} onClick={props.onCancel}>Cancel</button>
        <button type="button" onClick={props.onConfirm}>Archive</button>
      </header>
    </dialog>
  )
}

export function DeleteTaskDialog(props: { title: string; worktreePath: string | null; pending: boolean; onCancel: () => void; onConfirm: (removeWorktree: boolean) => void }) {
  const [removeWorktree, setRemoveWorktree] = createSignal(false)
  let cancel: HTMLButtonElement | undefined
  let dialog: HTMLElement | undefined
  onMount(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : undefined
    cancel?.focus()
    onCleanup(() => opener?.focus())
  })
  return <div class="picker-backdrop" onClick={() => { if (!props.pending) props.onCancel() }}>
  <section ref={dialog} class="picker picker-dialog" role="dialog" aria-modal="true" aria-label="Delete task"
    onClick={(event) => event.stopPropagation()}
    onKeyDown={(event) => {
      if (event.key === 'Escape' && !props.pending) { event.preventDefault(); props.onCancel(); return }
      if (event.key !== 'Tab' || !dialog) return
      const controls = Array.from(dialog.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled)'))
      if (!controls.length) return
      const index = controls.indexOf(document.activeElement as HTMLElement)
      if ((event.shiftKey && index <= 0) || (!event.shiftKey && index === controls.length - 1)) {
        event.preventDefault()
        ;(event.shiftKey ? controls.at(-1) : controls[0])?.focus()
      }
    }}>
    <header class="picker__header"><h3>Delete {props.title}?</h3></header>
    <div class="settings__project">
      <p>The task, conversation, and attachments will be deleted. Its branch will be kept.</p>
      <Show when={props.worktreePath}>
        {(path) => <label><input type="checkbox" checked={removeWorktree()} disabled={props.pending}
          onChange={(event) => setRemoveWorktree(event.currentTarget.checked)} /> Remove the clean worktree at <code>{path()}</code></label>}
      </Show>
      <p class="empty">Dirty worktrees cannot be removed.</p>
      <div class="terminal-pane__bar">
        <button type="button" ref={cancel} disabled={props.pending} onClick={props.onCancel}>Cancel</button>
        <button type="button" disabled={props.pending} onClick={() => props.onConfirm(removeWorktree())}>Delete task</button>
      </div>
    </div>
  </section></div>
}
