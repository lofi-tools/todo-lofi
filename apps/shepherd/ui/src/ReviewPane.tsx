import { For, Show, createResource, createSignal } from 'solid-js'

import { reviewFileDiff, reviewSnapshot, type ReviewAction, type TaskDetail } from './bridge'
import { GitDiffFile, GitDiffText } from './GitDiff'

export function ReviewPane(props: { task: TaskDetail; queuedNote?: string; openFile: (path: string) => void; onAction: (action: ReviewAction) => Promise<void> }) {
  const [review, { refetch }] = createResource(
    () => props.task.worktreePath ? [props.task.id, props.task.updatedAt] as const : false,
    ([taskId]) => reviewSnapshot(taskId),
  )

  const [selectedFile, setSelectedFile] = createSignal<string | null>(null)
  const [diff, { refetch: refetchDiff }] = createResource(
    () => {
      const path = selectedFile()
      return path && props.task.worktreePath ? [props.task.id, props.task.updatedAt, path] as const : false
    },
    ([taskId, , path]) => reviewFileDiff(taskId, path),
  )

  const [note, setNote] = createSignal('')
  const [pending, setPending] = createSignal(false)
  const running = () => ['starting', 'working', 'waiting', 'queued'].includes(props.task.state)
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
        <button type="button" onClick={() => { void refetch(); if (selectedFile()) void refetchDiff() }} disabled={review.loading || !props.task.worktreePath}>Refresh</button>
      </div>
      <Show when={props.task.state === 'done'}>
        <div class="terminal-pane__bar">
          <button type="button" disabled={pending()} onClick={() => void apply({ type: 'reopen' })}>Reopen</button>
        </div>
      </Show>
      <Show when={props.queuedNote}>
        {(queued) => (
          <div class="git-pane">
            <p class="empty">Queued review note</p>
            <pre class="git-pane__diff">{queued()}</pre>
            <div class="terminal-pane__bar">
              <Show when={props.task.state === 'ready' || props.task.state === 'review'}>
                <button type="button" disabled={pending()} onClick={() => void apply({ type: 'sendQueuedNote' })}>Send now</button>
              </Show>
              <button type="button" disabled={pending()} onClick={() => void apply({ type: 'removeQueuedNote' })}>Remove from queue</button>
            </div>
          </div>
        )}
      </Show>
      <Show when={!props.queuedNote && (running() || props.task.state === 'review')}>
        <form class="git-pane" onSubmit={(event) => {
          event.preventDefault()
          if (note().trim()) void apply({ type: 'sendNote', text: note() })
        }}>
          <textarea class="composer__input" aria-label="Review note" placeholder="Review note" value={note()} onInput={(event) => setNote(event.currentTarget.value)} />
          <button class="composer__send" type="submit" disabled={pending() || !note().trim()}>{running() ? 'Queue review note' : 'Send review note'}</button>
        </form>
      </Show>
      <Show when={props.task.state === 'review'}>
        <div class="terminal-pane__bar">
          <button type="button" disabled={pending()} onClick={() => void apply({ type: 'archive' })}>Mark reviewed and archive</button>
        </div>
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
                  <li>
                    <GitDiffFile file={file} selected={selectedFile() === file.path}
                      onSelect={() => setSelectedFile(file.path)} />
                  </li>
                )}</For>
              </ul>
            </Show>
            <Show when={selectedFile()}>
              {(path) => <>
                <Show when={snapshot().files.some((file) => file.path === path() && file.actionable && file.index !== 'D' && file.worktree !== 'D')}>
                  <div class="terminal-pane__bar"><button type="button" onClick={() => props.openFile(path())}>Open in Files</button></div>
                </Show>
                <Show when={diff.loading}><p role="status" class="empty">Loading diff…</p></Show>
                <Show when={diff.error}><p role="alert">{diff.error.message ?? String(diff.error)}</p></Show>
                <Show when={!diff.error && !diff.loading && diff() !== undefined}>
                  <GitDiffText label="File diff" text={diff() || 'No net content change.'} />
                </Show>
              </>}
            </Show>
          </>
        )}
      </Show>
    </section>
  )
}
