import { For, Show, onCleanup, onMount } from 'solid-js'

import type { TaskSummary } from './bridge'

export function TaskRows(props: {
  tasks: TaskSummary[]
  currentTaskId: string | null
  selection: () => Set<string> | undefined
  disabled?: boolean
  mac: boolean
  needsTerminalAttention: (taskId: string) => boolean
  onOpen: (task: TaskSummary) => void
  onSelect: (taskId: string, extend: boolean) => void
}) {
  return (
    <div class="tasks">
      <ul class="task-list">
        <For each={props.tasks}>
          {(task) => (
            <li class="task-list__row">
              <Show when={props.selection()}>
                {(selection) => (
                  <input type="checkbox" aria-label={`Select ${task.title}`} disabled={props.disabled}
                    checked={selection().has(task.id)} onClick={(event) => props.onSelect(task.id, event.shiftKey)} />
                )}
              </Show>
              <button type="button" class={`task task--${task.state}`}
                aria-current={task.id === props.currentTaskId ? 'true' : undefined}
                onClick={(event) => {
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
                <Show when={task.state === 'review' || task.state === 'waiting'}
                  fallback={<span class="task__state-label">{task.state}</span>}>
                  <span class="task__attention-badge">{task.state === 'review' ? 'Needs review' : 'Needs input'}</span>
                </Show>
              </button>
            </li>
          )}
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
