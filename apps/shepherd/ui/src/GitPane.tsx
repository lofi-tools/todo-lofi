import { For, Show, createResource, createSignal } from 'solid-js'

import { gitSnapshot, mutateGit, type GitMutation } from './bridge'

export function GitPane(props: { taskId: string; revision: number; live: boolean }) {
  const [git, { refetch, mutate }] = createResource(
    () => [props.taskId, props.revision] as const,
    ([taskId]) => gitSnapshot(taskId),
  )

  const [branch, setBranch] = createSignal('')
  const [message, setMessage] = createSignal('')
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal('')
  const disabled = () => props.live || pending() || git.loading

  async function apply(operation: GitMutation) {
    if (disabled()) return
    setPending(true)
    setError('')
    try {
      mutate(await mutateGit(props.taskId, operation))
      if (operation.type === 'commit') setMessage('')
      if (operation.type === 'checkout') setBranch('')
    } catch (failure) {
      setError((failure as { message?: string })?.message ?? String(failure))
      void refetch()
    } finally {
      setPending(false)
    }
  }

  return (
    <section class="git-pane" aria-label="Git worktree">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => void refetch()} disabled={git.loading}>Refresh</button>
      </div>
      <Show when={git.loading}><p class="empty" role="status">Loading Git status…</p></Show>
      <Show when={error()}><pre class="git-pane__diff" role="alert">{error()}</pre></Show>
      <Show when={git.error}><p role="alert">{git.error.message ?? String(git.error)}</p></Show>
      <Show when={!git.error && git()}>
        {(snapshot) => (
          <>
            <p>Branch: {snapshot().branch ?? 'Detached HEAD'}</p>
            <div class="terminal-pane__bar">
              <select aria-label="Local branch" value={branch()} onChange={(event) => setBranch(event.currentTarget.value)}>
                <option value="">Choose a branch</option>
                <For each={snapshot().branches}>{(name) => <option value={name}>{name}</option>}</For>
              </select>
              <button type="button" disabled={disabled() || !branch() || branch() === snapshot().branch || !!snapshot().files.length}
                onClick={() => void apply({ type: 'checkout', branch: branch() })}>Checkout</button>
            </div>
            <Show when={snapshot().files.length} fallback={<p class="empty">Working tree clean.</p>}>
              <ul class="git-pane__files">
                <For each={snapshot().files}>{(file) => (
                  <li>
                    <code>{file.index.trim() || '·'}{file.worktree.trim() || '·'}</code> {file.originalPath ? `${file.originalPath} → ${file.path}` : file.path}
                    <div class="terminal-pane__bar">
                      <Show when={file.actionable && file.worktree.trim()}>
                        <button type="button" disabled={disabled()} onClick={() => void apply({ type: 'stage', path: file.path })}>Stage</button>
                      </Show>
                      <Show when={file.actionable && file.index.trim() && file.index !== '?'}>
                        <button type="button" disabled={disabled()} onClick={() => void apply({ type: 'unstage', path: file.path })}>Unstage</button>
                      </Show>
                    </div>
                  </li>
                )}</For>
              </ul>
            </Show>
            <Show when={snapshot().files.some((file) => file.worktree.trim())}>
              <div class="terminal-pane__bar">
                <button type="button" disabled={disabled()} onClick={() => void apply({ type: 'stage', path: null })}>Stage all</button>
              </div>
            </Show>
            <form class="git-pane" onSubmit={(event) => {
              event.preventDefault()
              void apply({ type: 'commit', message: message(), expected: snapshot().stagedRevision })
            }}>
              <textarea class="composer__input" aria-label="Commit message" placeholder="Commit message" value={message()} onInput={(event) => setMessage(event.currentTarget.value)} />
              <button class="composer__send" type="submit" disabled={disabled() || !message().trim() || !snapshot().stagedDiff}>Commit staged changes</button>
            </form>
            <div class="terminal-pane__bar">
              <button type="button" disabled={disabled() || !!snapshot().mergeBlocked}
                onClick={() => void apply({ type: 'squashMerge' })}>Squash-merge into base</button>
            </div>
            <Show when={snapshot().mergeBlocked}>{(reason) => <p class="empty">{reason()}</p>}</Show>
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
