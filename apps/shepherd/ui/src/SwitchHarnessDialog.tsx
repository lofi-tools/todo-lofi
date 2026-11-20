import { Show } from 'solid-js'

import { Modal, ModalActions } from './Modal'

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
  return <Modal label="Switch harness" title={<>Switch to {props.to}?</>} onClose={props.onCancel} initialFocus={() => cancel}>
        <p>{props.to} starts a new session and cannot access {props.from}’s private context. Your conversation and worktree stay in place.</p>
        <div class="modal__option">
          <button type="button" class="modal__switch" role="switch" aria-checked={props.summarize} disabled={props.phase !== 'confirm' && props.phase !== 'error'}
            onClick={() => props.onSummarize(!props.summarize)}>Summarize context for the new harness<span class="project-menu__switch" aria-hidden="true" /></button>
          <p class="empty">The summary is prepared by {props.from} and added to your next prompt for review.</p>
        </div>
        <Show when={props.phase === 'waiting'}><p role="status">Waiting for the current response to finish…</p></Show>
        <Show when={props.phase === 'starting' || props.phase === 'summarizing'}><p role="status">Preparing the handoff summary…</p></Show>
        <Show when={props.phase === 'switching'}><p role="status">Switching harness…</p></Show>
        <Show when={props.error}><p role="alert">{props.error}</p></Show>
        <ModalActions>
          <button type="button" class="project-action" ref={cancel} disabled={props.phase === 'switching'} onClick={props.onCancel}>Cancel</button>
          <button type="button" class="project-action" disabled={props.phase !== 'confirm' && props.phase !== 'error'} onClick={props.onConfirm}>
            {props.phase === 'error' ? 'Try again' : 'Switch harness'}
          </button>
        </ModalActions>
  </Modal>
}
