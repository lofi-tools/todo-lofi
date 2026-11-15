import { Show, createEffect, createMemo, createResource, createSignal, onCleanup, onMount, type JSX } from 'solid-js'

import { gitFileDiff, gitSnapshot, mutateGit, type GitMutation } from './bridge'
import { GitDiffText } from './GitDiff'
import { ChangedFiles } from './ChangedFiles'
import { reportError } from './diagnostics'
import { PickerCaret, PickerMenu } from './PickerMenu'

interface GitRequest {
  taskId: string
  action: 'stageAll' | 'commit'
}

export function GitPane(props: {
  taskId: string; revision: number; fileRevision: number; live: boolean; openTerminal: () => void; openFile: (path: string) => void
  request?: GitRequest; onRequestHandled: (request: GitRequest) => void; commitShortcut?: string
  mode: 'files' | 'changes'; files: JSX.Element; worktree: string; worktreeLabel: string; baseBranch: string | null
}) {
  const taskId = createMemo(() => props.taskId)
  const revision = createMemo(() => props.revision)
  const [git, { refetch, mutate }] = createResource(
    () => [taskId(), revision(), props.fileRevision] as const,
    ([taskId]) => gitSnapshot(taskId),
  )

  const [message, setMessage] = createSignal('')
  const [amendRequested, setAmendRequested] = createSignal<string | null>(null)
  const [commitMethod, setCommitMethod] = createSignal<'commit' | 'amend'>('commit')
  const [selectedFile, setSelectedFile] = createSignal<string | null>(null)
  const [discardRequested, setDiscardRequested] = createSignal<string | null>(null)
  const [fileDiff, { refetch: refetchFile }] = createResource(
    () => {
      const path = selectedFile()
      return path ? [taskId(), revision(), path] as const : false
    },
    ([taskId, , path]) => gitFileDiff(taskId, path),
  )
  const [pending, setPending] = createSignal(false)
  const [notice, setNotice] = createSignal('')
  const [mergeMethod, setMergeMethod] = createSignal<'squashMerge' | 'mergeEach'>('squashMerge')
  const disabled = () => props.live || pending() || git.loading
  let commitMessage: HTMLTextAreaElement | undefined
  let gitContainer: HTMLElement | undefined
  let gitBlock: HTMLDivElement | undefined
  let gitObserver: ResizeObserver | undefined
  onMount(() => {
    gitObserver = new ResizeObserver(() => {
      if (gitContainer && gitBlock) gitContainer.style.setProperty('--git-overlay-height', `${gitBlock.offsetHeight + 8}px`)
    })
    if (gitBlock) gitObserver.observe(gitBlock)
    onCleanup(() => gitObserver?.disconnect())
  })

  createEffect(() => {
    const request = props.request
    if (request?.taskId !== props.taskId || git.loading) return
    props.onRequestHandled(request)
    if (git.error || disabled()) return
    if (request.action === 'commit') {
      commitMessage?.focus()
    } else if (git()?.files.some((file) => file.worktree.trim())) {
      void apply({ type: 'stage', path: null })
    } else {
      setNotice('No unstaged changes.')
    }
  })

  async function apply(operation: GitMutation) {
    if (disabled()) return
    setPending(true)
    setNotice('')
    try {
      const snapshot = await mutateGit(props.taskId, operation)
      mutate(snapshot)
      setDiscardRequested(null)
      if (snapshot.files.some((file) => file.path === selectedFile())) void refetchFile()
      else setSelectedFile(null)
      if (operation.type === 'commit' || operation.type === 'amend') { setMessage(''); setAmendRequested(null) }
      if (operation.type === 'squashMerge' || operation.type === 'mergeEach') {
        setNotice(`${operation.type === 'squashMerge' ? 'Squash-merged' : 'Merged'} into ${snapshot.projectRoot?.branch ?? 'the base branch'}.`)
      }
    } catch (failure) {
      reportError('mutate_git', failure)
      void refetch()
      void refetchFile()
    } finally {
      setPending(false)
    }
  }

  return (
    <section ref={gitContainer} class="explorer__git" aria-label="Git worktree">
      <div class="explorer__content" hidden={props.mode !== 'files'}>{props.files}</div>
      <div class="explorer__content explorer__changes" hidden={props.mode !== 'changes'}>
        <Show when={git.loading}><p class="empty" role="status">Loading Git status…</p></Show>
        <Show when={!git.error && git()}>{(snapshot) => <>
          <ChangedFiles files={snapshot().files} stats={snapshot().lineStats ?? []} selected={selectedFile()}
            onSelect={(path) => { setDiscardRequested(null); setSelectedFile(path) }} />
          <div class="terminal-pane__bar">
            <button type="button" onClick={() => { void refetch(); if (selectedFile()) void refetchFile() }} disabled={git.loading}>Refresh</button>
            <Show when={snapshot().files.some((file) => file.worktree.trim())}>
              <button type="button" disabled={disabled()} onClick={() => void apply({ type: 'stage', path: null })}>Stage all</button>
            </Show>
          </div>
          <Show when={selectedFile()}>{(name) => <>
            <Show when={snapshot().files.find((file) => file.path === name())}>{(file) => <div class="terminal-pane__bar">
              <Show when={file().actionable && file().worktree.trim()}><button type="button" disabled={disabled()} onClick={() => void apply({ type: 'stage', path: file().path })}>Stage</button></Show>
              <Show when={file().actionable && file().index.trim() && file().index !== '?'}><button type="button" disabled={disabled()} onClick={() => void apply({ type: 'unstage', path: file().path })}>Unstage</button></Show>
              <Show when={file().actionable && file().index !== 'D' && file().worktree !== 'D'}><button type="button" onClick={() => props.openFile(name())}>Open in Files</button></Show>
            </div>}</Show>
            <Show when={fileDiff.loading}><p class="empty" role="status">Loading file diff…</p></Show>
            <Show when={!fileDiff.loading && !fileDiff.error && fileDiff()}>{(diff) => <section class="git-pane" aria-label={`Diff for ${name()}`}>
              <Show when={diff().staged}><h3>Staged</h3><GitDiffText label="Staged diff" text={diff().staged} /></Show>
              <Show when={diff().unstaged}><h3>Unstaged</h3><GitDiffText label="Unstaged diff" text={diff().unstaged} /></Show>
              <Show when={!diff().staged && !diff().unstaged}><p class="empty">No tracked diff.</p></Show>
              <Show when={diff().canDiscard}><div class="terminal-pane__bar">
                <Show when={discardRequested() === diff().revision} fallback={<button type="button" disabled={disabled()} onClick={() => setDiscardRequested(diff().revision)}>Discard unstaged changes…</button>}>
                  <button type="button" disabled={disabled()} onClick={() => void apply({ type: 'discard', path: name(), expected: diff().revision })}>Discard changes to {name()}</button>
                  <button type="button" onClick={() => setDiscardRequested(null)}>Cancel</button>
                </Show>
              </div></Show>
            </section>}</Show>
          </>}</Show>
        </>}</Show>
      </div>
      <Show when={!git.error && git()}>{(snapshot) => (
        <div ref={(element) => { gitBlock = element; gitObserver?.observe(element) }} class="explorer__git-block">
          <div class="explorer__git-header">
            <p class="explorer__branches" title={`${snapshot().branch ?? 'Detached HEAD'} → ${props.baseBranch ?? 'base branch'}`}><strong>{snapshot().branch ?? 'Detached HEAD'}</strong> <span aria-hidden="true">→</span> {props.baseBranch ?? 'base branch'}</p>
            <div class="explorer__merge" role="group" aria-label="Merge controls">
              <button type="button" aria-label={`Merge into ${props.baseBranch ?? 'base branch'}`} disabled={disabled() || !!snapshot().mergeBlocked}
                onClick={() => void apply({ type: mergeMethod() })}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="5" cy="6" r="3"/><path d="M5 9v12"/><circle cx="19" cy="18" r="3"/><path d="m15 9l-3-3l3-3"/><path d="M12 6h5a2 2 0 0 1 2 2v7"/></svg>
                {mergeMethod() === 'squashMerge' ? 'Squash-merge' : 'Merge each'}
              </button>
              <PickerMenu label="Merge method" value={mergeMethod()} options={[
                { id: 'squashMerge', name: 'Squash-merge', detail: 'Squash commits into one' },
                { id: 'mergeEach', name: 'Merge each', detail: 'Merge each commit' },
              ]} disabled={disabled()} onChange={(method) => setMergeMethod(method === 'mergeEach' ? 'mergeEach' : 'squashMerge')}
                triggerClass="explorer__split-toggle" above align="end"><PickerCaret /></PickerMenu>
            </div>
          </div>
          <p class="empty explorer__worktree" title={props.worktree}>{props.worktreeLabel}</p>
            <form class="explorer__commit" data-git-commit onSubmit={(event) => {
              event.preventDefault()
              if (commitMethod() === 'amend') setAmendRequested(snapshot().stagedRevision)
              else void apply({ type: 'commit', message: message(), expected: snapshot().stagedRevision })
            }}>
              <textarea ref={commitMessage} aria-label="Commit message" placeholder="Commit message" value={message()} onInput={(event) => setMessage(event.currentTarget.value)} />
              <div class="explorer__commit-footer">
                <Show when={commitMethod() === 'amend' && amendRequested() === snapshot().stagedRevision} fallback={
                  <div class="explorer__commit-actions" role="group" aria-label="Commit controls">
                    <button type="submit" aria-keyshortcuts={props.commitShortcut}
                      disabled={disabled() || !message().trim() || (commitMethod() === 'commit' ? !snapshot().stagedDiff : !snapshot().branch)}>
                      {commitMethod() === 'commit' ? 'Commit' : 'Amend'}
                    </button>
                    <PickerMenu label="Commit action" value={commitMethod()} options={[
                      { id: 'commit', name: 'Commit' }, { id: 'amend', name: 'Amend' },
                    ]} disabled={disabled()} onChange={(method) => { setCommitMethod(method === 'amend' ? 'amend' : 'commit'); setAmendRequested(null) }}
                      triggerClass="explorer__split-toggle" above align="end"><PickerCaret /></PickerMenu>
                  </div>
                }>
                  <span class="explorer__amend-confirmation">
                    <button type="button" disabled={disabled() || !message().trim() || !snapshot().branch}
                      onClick={() => void apply({ type: 'amend', message: message(), expected: snapshot().stagedRevision })}>
                      Replace tip on {snapshot().branch}
                    </button>
                    <button type="button" onClick={() => setAmendRequested(null)}>Cancel</button>
                  </span>
                </Show>
              </div>
            </form>
            <Show when={snapshot().mergeBlocked && snapshot().mergeBlocked !== 'The project root has uncommitted changes.'}>{(reason) => <p class="empty">{reason()}</p>}</Show>

          <Show when={notice()}>{(message) => <p class="empty" role="status">{message()}</p>}</Show>
        </div>
      )}</Show>
    </section>
  )
}
