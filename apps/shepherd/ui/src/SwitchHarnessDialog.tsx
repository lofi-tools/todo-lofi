import { Show, onCleanup, onMount } from 'solid-js'

export function SwitchHarnessDialog(props: {
  from: string
  to: string
  summarize: boolean
  phase: 'confirm' | 'waiting' | 'starting' | 'summarizing' | 'switching' | 'error'
  error?: string
  onSummarize: (value: boolean) => void
  onConfirm: () => void
  onCancel: () => void
}) {
  let cancel: HTMLButtonElement | undefined
  let dialog: HTMLElement | undefined
  onMount(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : undefined
    cancel?.focus()
    onCleanup(() => opener?.focus())
  })
  return <div class="picker-backdrop" onClick={props.onCancel}>
    <section ref={dialog} class="picker picker-dialog" role="dialog" aria-modal="true" aria-label="Switch harness"
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => {
        if (event.key === 'Escape') { event.preventDefault(); props.onCancel(); return }
        if (event.key !== 'Tab' || !dialog) return
        const controls = Array.from(dialog.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled)'))
        if (!controls.length) return
        const index = controls.indexOf(document.activeElement as HTMLElement)
        if ((event.shiftKey && index <= 0) || (!event.shiftKey && index === controls.length - 1)) {
          event.preventDefault()
          ;(event.shiftKey ? controls.at(-1) : controls[0])?.focus()
        }
      }}>
      <header class="picker__header"><h3>Switch to {props.to}?</h3></header>
      <div class="settings__project">
        <p>{props.to} starts a new session and cannot access {props.from}’s private context. Your conversation and worktree stay in place.</p>
        <label><input type="checkbox" checked={props.summarize} disabled={props.phase !== 'confirm' && props.phase !== 'error'}
          onChange={(event) => props.onSummarize(event.currentTarget.checked)} /> Summarize context for the new harness</label>
        <p class="empty">The summary is prepared by {props.from} and added to your next prompt for review.</p>
        <Show when={props.phase === 'waiting'}><p role="status">Waiting for the current response to finish…</p></Show>
        <Show when={props.phase === 'starting' || props.phase === 'summarizing'}><p role="status">Preparing the handoff summary…</p></Show>
        <Show when={props.phase === 'switching'}><p role="status">Switching harness…</p></Show>
        <Show when={props.error}><p role="alert">{props.error}</p></Show>
        <div class="terminal-pane__bar">
          <button type="button" ref={cancel} disabled={props.phase === 'switching'} onClick={props.onCancel}>Cancel</button>
          <button type="button" disabled={props.phase !== 'confirm' && props.phase !== 'error'} onClick={props.onConfirm}>
            {props.phase === 'error' ? 'Try again' : 'Switch harness'}
          </button>
        </div>
      </div>
    </section>
  </div>
}
