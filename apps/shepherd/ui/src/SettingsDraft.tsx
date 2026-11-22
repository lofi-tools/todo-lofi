import { Show, createContext, createSignal, onCleanup, useContext, type Accessor } from 'solid-js'

export interface SettingsDraft {
  dirty: Accessor<boolean>
  saving: Accessor<boolean>
  save: () => Promise<boolean>
  discard: () => void
}

export const SettingsDraftContext = createContext<{
  mac: boolean
  register: (draft: SettingsDraft) => () => void
}>()

export function createSettingsDraft(options: {
  dirty: Accessor<boolean>
  editable: Accessor<boolean>
  save: () => Promise<void>
  discard: () => void
}) {
  const context = useContext(SettingsDraftContext)
  const [saving, setSaving] = createSignal(false)
  const [error, setError] = createSignal('')
  let pending: Promise<boolean> | undefined

  async function submit() {
    setSaving(true)
    setError('')
    try {
      await options.save()
      return true
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : (failure as { message?: string })?.message ?? String(failure))
      return false
    } finally {
      setSaving(false)
    }
  }

  const draft = {
    dirty: options.dirty,
    saving,
    save() {
      if (pending) return pending
      if (!options.dirty()) return Promise.resolve(true)
      if (!options.editable()) {
        setError('Settings are unavailable for editing.')
        return Promise.resolve(false)
      }
      pending = submit().finally(() => { pending = undefined })
      return pending
    },
    discard() {
      options.discard()
      setError('')
    },
    editable: options.editable,
    error,
    mac: context?.mac ?? false,
    onKeyDown(event: KeyboardEvent) {
      if (event.isComposing || event.key !== 'Enter' || !(context?.mac ? event.metaKey : event.ctrlKey)) return
      event.preventDefault()
      event.stopPropagation()
      void draft.save()
    },
  }
  if (context) onCleanup(context.register(draft))
  return draft
}

export function SettingsSave(props: { draft: ReturnType<typeof createSettingsDraft>; label: string }) {
  return <>
    <div class="settings__save">
      <button class="project-action" type="button" aria-keyshortcuts={props.draft.mac ? 'Meta+Enter' : 'Control+Enter'}
        disabled={!props.draft.editable() || props.draft.saving() || !props.draft.dirty()} onClick={() => void props.draft.save()}>
        {props.label}
      </button>
      <kbd>{props.draft.mac ? '⌘↵' : 'Ctrl+Enter'}</kbd>
      <Show when={props.draft.saving()} fallback={<Show when={props.draft.dirty()}><span role="status">Unsaved edits</span></Show>}>
        <span role="status">Saving…</span>
      </Show>
    </div>
    <Show when={props.draft.error()}><p role="alert">{props.draft.error()}</p></Show>
  </>
}
