import { For, Show, createResource } from 'solid-js'

import { gitSnapshot } from './bridge'

export function GitPane(props: { taskId: string; revision: number }) {
  const [git, { refetch }] = createResource(
    () => [props.taskId, props.revision] as const,
    ([taskId]) => gitSnapshot(taskId),
  )

  return (
    <section class="git-pane" aria-label="Git worktree">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => void refetch()} disabled={git.loading}>Refresh</button>
      </div>
      <Show when={git.loading}><p class="empty" role="status">Loading Git status…</p></Show>
      <Show when={git.error}><p role="alert">{git.error.message ?? String(git.error)}</p></Show>
      <Show when={!git.error && git()}>
        {(snapshot) => (
          <>
            <p>Branch: {snapshot().branch ?? 'Detached HEAD'}</p>
            <Show when={snapshot().files.length} fallback={<p class="empty">Working tree clean.</p>}>
              <ul class="git-pane__files">
                <For each={snapshot().files}>{(file) => (
                  <li><code>{file.index.trim() || '·'}{file.worktree.trim() || '·'}</code> {file.path}</li>
                )}</For>
              </ul>
            </Show>
            <Show when={snapshot().stagedDiff}>
              <details><summary>Staged diff</summary><pre class="git-pane__diff">{snapshot().stagedDiff}</pre></details>
            </Show>
            <Show when={snapshot().unstagedDiff}>
              <details><summary>Unstaged diff</summary><pre class="git-pane__diff">{snapshot().unstagedDiff}</pre></details>
            </Show>
          </>
        )}
      </Show>
    </section>
  )
}
