import { For, Show, createResource, createSignal } from 'solid-js'

import { gitFileDiff, gitSnapshot, mutateGit, type GitMutation } from './bridge'

export function GitPane(props: { taskId: string; revision: number; live: boolean; openTerminal: () => void }) {
  const [git, { refetch, mutate }] = createResource(
    () => [props.taskId, props.revision] as const,
    ([taskId]) => gitSnapshot(taskId),
  )

  const [branch, setBranch] = createSignal('')
  const [newBranch, setNewBranch] = createSignal('')
  const [message, setMessage] = createSignal('')
  const [amendRequested, setAmendRequested] = createSignal<string | null>(null)
  const [selectedFile, setSelectedFile] = createSignal<string | null>(null)
  const [discardRequested, setDiscardRequested] = createSignal<string | null>(null)
  const [fileDiff, { refetch: refetchFile }] = createResource(
    () => {
      const path = selectedFile()
      return path ? [props.taskId, props.revision, path] as const : false
    },
    ([taskId, , path]) => gitFileDiff(taskId, path),
  )
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal('')
  const [notice, setNotice] = createSignal('')
  const disabled = () => props.live || pending() || git.loading

  async function apply(operation: GitMutation) {
    if (disabled()) return
    setPending(true)
    setError('')
    setNotice('')
    try {
      const snapshot = await mutateGit(props.taskId, operation)
      mutate(snapshot)
      setDiscardRequested(null)
      if (snapshot.files.some((file) => file.path === selectedFile())) void refetchFile()
      else setSelectedFile(null)
      if (operation.type === 'commit' || operation.type === 'amend') { setMessage(''); setAmendRequested(null) }
      if (operation.type === 'createBranch') setNewBranch('')
      if (operation.type === 'checkout') setBranch('')
      if (operation.type === 'squashMerge') {
        setNotice(`Squash-merged into ${snapshot.projectRoot?.branch ?? 'the base branch'}.`)
      }
    } catch (failure) {
      setError((failure as { message?: string })?.message ?? String(failure))
      void refetch()
      void refetchFile()
    } finally {
      setPending(false)
    }
  }

  return (
    <section class="git-pane" aria-label="Git worktree">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => { void refetch(); if (selectedFile()) void refetchFile() }} disabled={git.loading}>Refresh</button>
      </div>
      <Show when={git.loading}><p class="empty" role="status">Loading Git status…</p></Show>
      <Show when={error()}><pre class="git-pane__diff" role="alert">{error()}</pre></Show>
      <Show when={git.error}><p role="alert">{git.error.message ?? String(git.error)}</p></Show>
      <Show when={!git.error && git()}>
        {(snapshot) => (
          <>
            <p>Task branch: {snapshot().branch ?? 'Detached HEAD'}</p>
            <Show when={snapshot().tracking}>{(tracking) => <p class="empty">Upstream {tracking()}</p>}</Show>
            <Show when={snapshot().projectRoot}>
              {(root) => (
                <details>
                  <summary>Project root · {root().branch ?? 'Detached HEAD'} · {root().files.length} changed</summary>
                  <Show when={root().tracking}>{(tracking) => <p class="empty">Upstream {tracking()}</p>}</Show>
                  <ul class="git-pane__files">
                    <For each={root().files}>{(file) => (
                      <li><code>{file.index.trim() || '·'}{file.worktree.trim() || '·'}</code>{' '}
                        {file.originalPath ? `${file.originalPath} → ${file.path}` : file.path}
                      </li>
                    )}</For>
                  </ul>
                  <div class="terminal-pane__bar"><button type="button" onClick={props.openTerminal}>Open project terminals</button></div>
                </details>
              )}
            </Show>
            <div class="terminal-pane__bar">
              <select aria-label="Local branch" value={branch()} onChange={(event) => setBranch(event.currentTarget.value)}>
                <option value="">Choose a branch</option>
                <For each={snapshot().branches}>{(name) => <option value={name}>{name}</option>}</For>
              </select>
              <button type="button" disabled={disabled() || !branch() || branch() === snapshot().branch || !!snapshot().files.length}
                onClick={() => void apply({ type: 'checkout', branch: branch() })}>Checkout</button>
            </div>
            <form class="terminal-pane__bar" onSubmit={(event) => {
              event.preventDefault()
              const name = newBranch().trim()
              if (name && !snapshot().files.length) void apply({ type: 'createBranch', branch: name })
            }}>
              <input class="composer__input" aria-label="New branch name" placeholder="New branch" value={newBranch()} onInput={(event) => setNewBranch(event.currentTarget.value)} />
              <button type="submit" disabled={disabled() || !newBranch().trim() || !!snapshot().files.length}>Create and check out</button>
            </form>
            <Show when={snapshot().files.length} fallback={<p class="empty">Working tree clean.</p>}>
              <ul class="git-pane__files">
                <For each={snapshot().files}>{(file) => (
                  <li>
                    <code>{file.index.trim() || '·'}{file.worktree.trim() || '·'}</code>{' '}
                    <button class="git-pane__file" type="button" disabled={!file.actionable}
                      aria-pressed={selectedFile() === file.path}
                      onClick={() => { setDiscardRequested(null); setSelectedFile(file.path) }}>
                      {file.originalPath ? `${file.originalPath} → ${file.path}` : file.path}
                    </button>
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
            <Show when={selectedFile()}>
              {(name) => (
                <>
                  <Show when={fileDiff.loading}><p class="empty" role="status">Loading file diff…</p></Show>
                  <Show when={fileDiff.error}><p role="alert">{fileDiff.error.message ?? String(fileDiff.error)}</p></Show>
                  <Show when={!fileDiff.loading && !fileDiff.error && fileDiff()}>
                    {(diff) => (
                      <section class="git-pane" aria-label={`Diff for ${name()}`}>
                        <Show when={diff().staged}>
                          <h3>Staged</h3><pre class="git-pane__diff">{diff().staged}</pre>
                        </Show>
                        <Show when={diff().unstaged}>
                          <h3>Unstaged</h3><pre class="git-pane__diff">{diff().unstaged}</pre>
                        </Show>
                        <Show when={!diff().staged && !diff().unstaged}><p class="empty">No tracked diff.</p></Show>
                        <Show when={diff().canDiscard}>
                          <div class="terminal-pane__bar">
                            <Show when={discardRequested() === diff().revision} fallback={
                              <button type="button" disabled={disabled()} onClick={() => setDiscardRequested(diff().revision)}>Discard unstaged changes…</button>
                            }>
                              <button type="button" disabled={disabled()}
                                onClick={() => void apply({ type: 'discard', path: name(), expected: diff().revision })}>Discard changes to {name()}</button>
                              <button type="button" onClick={() => setDiscardRequested(null)}>Cancel</button>
                            </Show>
                          </div>
                        </Show>
                      </section>
                    )}
                  </Show>
                </>
              )}
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
              <Show when={amendRequested() !== snapshot().stagedRevision} fallback={
                <div class="terminal-pane__bar">
                  <button type="button" disabled={disabled() || !message().trim() || !snapshot().branch}
                    onClick={() => void apply({ type: 'amend', message: message(), expected: snapshot().stagedRevision })}>
                    Replace tip on {snapshot().branch}
                  </button>
                  <button type="button" onClick={() => setAmendRequested(null)}>Cancel</button>
                </div>
              }>
                <button type="button" class="composer__send" disabled={disabled() || !snapshot().branch || !message().trim()}
                  onClick={() => setAmendRequested(snapshot().stagedRevision)}>Amend tip…</button>
              </Show>
            </form>
            <div class="terminal-pane__bar">
              <button type="button" disabled={disabled() || !!snapshot().mergeBlocked}
                onClick={() => void apply({ type: 'squashMerge' })}>Squash-merge into base</button>
            </div>
            <Show when={snapshot().mergeBlocked}>{(reason) => <p class="empty">{reason()}</p>}</Show>
            <Show when={notice()}>{(message) => <p class="empty" role="status">{message()}</p>}</Show>
            <details>
              <summary>Recent commits</summary>
              <ol class="git-pane__files">
                <For each={snapshot().history}>{(commit) => (
                  <li><code title={commit.id}>{commit.id.slice(0, 7)}</code> {commit.subject} <small>{commit.author} · {commit.time}</small></li>
                )}</For>
              </ol>
            </details>
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
