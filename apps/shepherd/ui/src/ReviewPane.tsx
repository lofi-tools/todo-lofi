import { For, Show, createResource, createSignal } from 'solid-js'

import { reviewSnapshot, type ReviewAction, type TaskDetail } from './bridge'

export function ReviewPane(props: { task: TaskDetail; onAction: (action: ReviewAction) => Promise<void> }) {
  const [review, { refetch }] = createResource(
    () => props.task.worktreePath ? [props.task.id, props.task.updatedAt] as const : false,
    ([taskId]) => reviewSnapshot(taskId),
  )

  const [note, setNote] = createSignal('')
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal('')

  async function apply(action: ReviewAction) {
    if (pending()) return
    setPending(true)
    setError('')
    try {
      await props.onAction(action)
      if (action.type === 'sendNote') setNote('')
    } catch (failure) {
      setError((failure as { message?: string })?.message ?? String(failure))
    } finally {
      setPending(false)
    }
  }

  return (
    <section class="git-pane" aria-label="Task changes">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => void refetch()} disabled={review.loading || !props.task.worktreePath}>Refresh</button>
      </div>
      <Show when={props.task.state === 'done'}>
        <div class="terminal-pane__bar">
          <button type="button" disabled={pending()} onClick={() => void apply({ type: 'reopen' })}>Reopen</button>
        </div>
      </Show>
      <Show when={props.task.state === 'review'}>
        <form class="git-pane" onSubmit={(event) => {
          event.preventDefault()
          if (note().trim()) void apply({ type: 'sendNote', text: note() })
        }}>
          <textarea class="composer__input" aria-label="Review note" placeholder="Review note" value={note()} onInput={(event) => setNote(event.currentTarget.value)} />
          <button class="composer__send" type="submit" disabled={pending() || !note().trim()}>Send review note</button>
          <button class="composer__send" type="button" disabled={pending()} onClick={() => void apply({ type: 'archive' })}>Mark reviewed and archive</button>
        </form>
      </Show>
      <Show when={error()}>{(failure) => <p role="alert">{failure()}</p>}</Show>
      <Show when={review.loading}><p class="empty" role="status">Loading changes…</p></Show>
      <Show when={review.error}><p role="alert">{review.error.message ?? String(review.error)}</p></Show>
      <Show when={!review.error && review()}>
        {(snapshot) => (
          <>
            <Show when={snapshot().files.length} fallback={<p class="empty">No changed files.</p>}>
              <ul class="git-pane__files">
                <For each={snapshot().files}>{(file) => (
                  <li><code>{file.index.trim() || '·'}{file.worktree.trim() || '·'}</code> {file.originalPath ? `${file.originalPath} → ${file.path}` : file.path}</li>
                )}</For>
              </ul>
            </Show>
            <Show when={snapshot().diff}><pre class="git-pane__diff">{snapshot().diff}</pre></Show>
          </>
        )}
      </Show>
    </section>
  )
}
