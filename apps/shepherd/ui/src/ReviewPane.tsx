import { For, Show, createResource } from 'solid-js'

import { reviewSnapshot } from './bridge'

export function ReviewPane(props: { taskId: string; revision: number }) {
  const [review, { refetch }] = createResource(
    () => [props.taskId, props.revision] as const,
    ([taskId]) => reviewSnapshot(taskId),
  )

  return (
    <section class="git-pane" aria-label="Task changes">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => void refetch()} disabled={review.loading}>Refresh</button>
      </div>
      <Show when={review.loading}><p class="empty" role="status">Loading changes…</p></Show>
      <Show when={review.error}><p role="alert">{review.error.message ?? String(review.error)}</p></Show>
      <Show when={!review.error && review()}>
        {(snapshot) => (
          <>
            <Show when={snapshot().files.length} fallback={<p class="empty">No changed files.</p>}>
              <ul class="git-pane__files">
                <For each={snapshot().files}>{(file) => (
                  <li><code>{file.index.trim() || '·'}{file.worktree.trim() || '·'}</code> {file.path}</li>
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
