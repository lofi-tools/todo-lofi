import { For, Show, createSignal, onCleanup, onMount } from 'solid-js'

import { diagnosticHistory, reportError, type Diagnostic } from './diagnostics'

export function ErrorNotifications() {
  const [toast, setToast] = createSignal<Diagnostic>()
  const [history, setHistory] = createSignal<Diagnostic[]>([])
  const [open, setOpen] = createSignal(false)
  const [historyError, setHistoryError] = createSignal('')
  let timer: number | undefined

  onMount(() => {
    const receive = (event: Event) => {
      const record = (event as CustomEvent<Diagnostic>).detail
      setToast(record)
      setHistory((current) => [...current.slice(-199), record])
      if (timer !== undefined) window.clearTimeout(timer)
      timer = window.setTimeout(() => setToast(undefined), 7000)
    }
    const error = (event: ErrorEvent) => reportError('window', event.error ?? event.message)
    const rejection = (event: PromiseRejectionEvent) => reportError('unhandled promise', event.reason)
    window.addEventListener('shepherd-diagnostic', receive)
    window.addEventListener('error', error)
    window.addEventListener('unhandledrejection', rejection)
    onCleanup(() => {
      window.removeEventListener('shepherd-diagnostic', receive)
      window.removeEventListener('error', error)
      window.removeEventListener('unhandledrejection', rejection)
      if (timer !== undefined) window.clearTimeout(timer)
    })
  })

  async function showHistory() {
    setOpen(true)
    setHistoryError('')
    try {
      const records = await diagnosticHistory()
      if (Array.isArray(records)) setHistory((current) => {
        const combined = new Map(records.map((record) => [record.id, record]))
        for (const record of current) combined.set(record.id, record)
        return [...combined.values()].slice(-200)
      })
    } catch (error) {
      setHistoryError(`Could not load error history: ${String(error)}`)
    }
  }

  return <>
    <button type="button" class="status-bar__filter" onClick={() => void showHistory()}>Error history</button>
    <Show when={toast()}>{(record) => <div class="error-toast" role="alert">
      <span>{record().message}</span>
      <button type="button" onClick={() => void showHistory()}>Details</button>
      <button type="button" aria-label="Dismiss error" onClick={() => setToast(undefined)}>×</button>
    </div>}</Show>
    <Show when={open()}>
      <div class="picker-backdrop error-history-backdrop" onClick={() => setOpen(false)}>
        <section class="picker error-history" role="dialog" aria-modal="true" aria-label="Error history" onClick={(event) => event.stopPropagation()}
          onKeyDown={(event) => { if (event.key === 'Escape') { event.stopPropagation(); setOpen(false) } }}>
          <header class="picker__header"><h3>Error history</h3><button type="button" onClick={() => setOpen(false)}>Close</button></header>
          <Show when={historyError()}>{(message) => <p>{message()}</p>}</Show>
          <Show when={history().length} fallback={<p class="empty">No recorded errors.</p>}>
            <For each={[...history()].reverse()}>{(record) => <details>
              <summary>{record.message}</summary>
              <p>{record.time} · {record.operation} · {record.id}</p>
              <pre>{record.details}</pre>
            </details>}</For>
          </Show>
        </section>
      </div>
    </Show>
  </>
}
