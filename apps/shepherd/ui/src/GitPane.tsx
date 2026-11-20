import { Show, createEffect, createMemo, createResource, createSignal, onCleanup, onMount, type JSX } from 'solid-js'

import { gitFileDiff, gitSnapshot, mutateGit, type GitMutation, type GitSnapshot } from './bridge'
import { GitDiffText } from './GitDiff'
import { ChangedFiles } from './ChangedFiles'
import { reportError } from './diagnostics'
import { PickerCaret, PickerMenu } from './PickerMenu'
import { SegmentedControl } from './SegmentedControl'
import type { SendAccent } from './SettingsPane'

interface GitRequest {
  taskId: string
  action: 'stageAll' | 'commit'
}

const GIT_PANE_CACHE_KEY = 'shepherd.git-pane.v1'
const GIT_PANE_CACHE_LIMIT = 16
const GIT_PANE_ENTRY_LIMIT = 48_000

type GitPaneDisplay = Pick<GitSnapshot, 'branch'> & Partial<Pick<GitSnapshot, 'files' | 'lineStats'>>
type CachedGitPane = { scope: string; display: GitPaneDisplay }

function readGitPaneCache(): CachedGitPane[] {
  try {
    const entries: unknown = JSON.parse(localStorage.getItem(GIT_PANE_CACHE_KEY) ?? '[]')
    return Array.isArray(entries) ? entries.filter((entry): entry is CachedGitPane =>
      typeof entry?.scope === 'string' && entry.display &&
      (typeof entry.display.branch === 'string' || entry.display.branch === null) &&
      (entry.display.files === undefined || (Array.isArray(entry.display.files) &&
        entry.display.files.every((file: unknown) => {
          const item = file as Partial<GitSnapshot['files'][number]> | null
          return typeof item?.path === 'string' && typeof item.index === 'string' &&
            typeof item.worktree === 'string' && typeof item.actionable === 'boolean'
        }))) &&
      (entry.display.lineStats === undefined || (Array.isArray(entry.display.lineStats) &&
        entry.display.lineStats.every((stat: unknown) => stat !== null && typeof stat === 'object' &&
          'path' in stat && typeof stat.path === 'string'))),
    ) : []
  } catch {
    return []
  }
}

function saveGitPaneDisplay(scope: string, snapshot: GitSnapshot): GitPaneDisplay {
  const { branch, files, lineStats } = snapshot
  const display = { branch, files, lineStats }
  const stored = JSON.stringify(display).length <= GIT_PANE_ENTRY_LIMIT ? display : { branch }
  try {
    const previous = readGitPaneCache()
    if (previous[0]?.scope !== scope || JSON.stringify(previous[0].display) !== JSON.stringify(stored)) {
      localStorage.setItem(GIT_PANE_CACHE_KEY, JSON.stringify([
        { scope, display: stored }, ...previous.filter((entry) => entry.scope !== scope).slice(0, GIT_PANE_CACHE_LIMIT - 1),
      ]))
    }
  } catch (failure) {
    console.warn('Could not cache the Git pane', failure)
  }
  return display
}

export function GitPane(props: {
  taskId: string; revision: number; fileRevision: number; live: boolean; openTerminal: () => void; openFile: (path: string) => void
  request?: GitRequest; onRequestHandled: (request: GitRequest) => void; commitShortcut?: string; sendAccent: SendAccent
  mode: 'files' | 'changes'; onModeChange: (mode: 'files' | 'changes') => void; files: JSX.Element; worktree: string; worktreeLabel: string; baseBranch: string | null
}) {
  const cacheScope = JSON.stringify([props.taskId, props.worktree])
  const [cached, setCached] = createSignal(readGitPaneCache().find((entry) => entry.scope === cacheScope)?.display)
  const taskId = createMemo(() => props.taskId)
  const revision = createMemo(() => props.revision)
  const [git, { refetch, mutate }] = createResource(
    () => [taskId(), revision(), props.fileRevision] as const,
    ([taskId]) => gitSnapshot(taskId),
  )

  const fresh = () => git.error || git.loading ? undefined : git.latest
  const display = () => fresh() ?? cached()
  const syncFailed = () => !git.loading && !!git.error
  createEffect(() => {
    const snapshot = fresh()
    if (snapshot) setCached(saveGitPaneDisplay(cacheScope, snapshot))
  })

  const [message, setMessage] = createSignal('')
  const [amendRequested, setAmendRequested] = createSignal<string | null>(null)
  const [commitMethod, setCommitMethod] = createSignal<'commit' | 'amend'>('commit')
  const [selectedFile, setSelectedFile] = createSignal<string | null>(null)
  const [discardRequested, setDiscardRequested] = createSignal<string | null>(null)
  const [fileDiff, { refetch: refetchFile }] = createResource(
    () => {
      const path = selectedFile()
      return path && fresh()?.files.some((file) => file.path === path) ? [taskId(), revision(), path] as const : false
    },
    ([taskId, , path]) => gitFileDiff(taskId, path),
  )
  const [pending, setPending] = createSignal(false)
  const [notice, setNotice] = createSignal('')
  const [mergeMethod, setMergeMethod] = createSignal<'squashMerge' | 'mergeEach'>('squashMerge')
  const disabled = () => props.live || pending() || !fresh()
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
    } else if (fresh()?.files.some((file) => file.worktree.trim())) {
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
      <div class="explorer__toolbar">
        <SegmentedControl label="File explorer view" value={props.mode} choices={[{ value: 'files', label: 'Files' }, { value: 'changes', label: 'Changes' }]} onChange={props.onModeChange} />
        <div class="explorer__toolbar-actions">
          <Show when={display()}>
            <div class="explorer__merge" role="group" aria-label="Merge controls">
              <button type="button" aria-label={`Merge into ${props.baseBranch ?? 'base branch'}`} title={`Merge into ${props.baseBranch ?? 'base branch'}`}
                disabled={disabled() || !!fresh()?.mergeBlocked}
                onClick={() => void apply({ type: mergeMethod() })}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="5" cy="6" r="3"/><path d="M5 9v12"/><circle cx="19" cy="18" r="3"/><path d="m15 9l-3-3l3-3"/><path d="M12 6h5a2 2 0 0 1 2 2v7"/></svg>
                <span class="explorer__merge-label">{mergeMethod() === 'squashMerge' ? 'Squash-merge' : 'Merge each'}</span>
              </button>
              <PickerMenu label="Merge method" value={mergeMethod()} options={[
                { id: 'squashMerge', name: 'Squash-merge', detail: 'Squash commits into one' },
                { id: 'mergeEach', name: 'Merge each', detail: 'Merge each commit' },
              ]} disabled={disabled()} onChange={(method) => setMergeMethod(method === 'mergeEach' ? 'mergeEach' : 'squashMerge')}
                triggerClass="explorer__split-toggle" align="end"><PickerCaret /></PickerMenu>
            </div>
          </Show>
          <Show when={props.mode === 'changes'}><button type="button" class="project-action project-action--icon" aria-label="Refresh changes" title="Refresh changes" disabled={git.loading} onClick={() => { void refetch(); if (selectedFile()) void refetchFile() }}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 7v5h-5" /><path d="M20 12a8 8 0 1 0-2.34 5.66" /></svg>
          </button></Show>
        </div>
      </div>
      <Show when={!display()}>
        <Show when={!syncFailed()} fallback={<div class="terminal-pane__bar">
          <p class="empty" role="alert">Could not load Git status.</p>
          <button type="button" onClick={() => void refetch()}>Retry</button>
        </div>}>
          <p class="empty" role="status">Loading Git status…</p>
        </Show>
      </Show>
      <div class="explorer__content" hidden={props.mode !== 'files'}>{props.files}</div>
      <div class="explorer__content explorer__changes" hidden={props.mode !== 'changes'}>
        <Show when={display()?.files}>{(files) => <>
          <ChangedFiles files={files()} stats={display()?.lineStats ?? []} selected={selectedFile()}
            onSelect={(path) => { setDiscardRequested(null); setSelectedFile(path) }} />
          <Show when={files().some((file) => file.worktree.trim())}>
            <div class="terminal-pane__bar">
              <button type="button" disabled={disabled()} onClick={() => void apply({ type: 'stage', path: null })}>Stage all</button>
            </div>
          </Show>
          <Show when={selectedFile()}>{(name) => <>
            <Show when={files().find((file) => file.path === name())}>{(file) => <div class="terminal-pane__bar">
              <Show when={file().actionable && file().worktree.trim()}><button type="button" disabled={disabled()} onClick={() => void apply({ type: 'stage', path: file().path })}>Stage</button></Show>
              <Show when={file().actionable && file().index.trim() && file().index !== '?'}><button type="button" disabled={disabled()} onClick={() => void apply({ type: 'unstage', path: file().path })}>Unstage</button></Show>
              <Show when={file().actionable && file().index !== 'D' && file().worktree !== 'D'}><button type="button" onClick={() => props.openFile(name())}>Open in Files</button></Show>
            </div>}</Show>
            <Show when={fresh() && fileDiff.loading}><p class="empty" role="status">Loading file diff…</p></Show>
            <Show when={fresh()?.files.some((file) => file.path === name()) && !fileDiff.loading && !fileDiff.error && fileDiff()}>{(diff) => <section class="git-pane" aria-label={`Diff for ${name()}`}>
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
      <Show when={display()}>{(snapshot) => (
        <div ref={(element) => { gitBlock = element; gitObserver?.observe(element) }} class="explorer__git-block">
          <div class="explorer__git-details">
            <p class="explorer__branches" title={`${snapshot().branch ?? '(no branch)'} → ${props.baseBranch ?? 'base branch'}`}><strong>{snapshot().branch ?? '(no branch)'}</strong> <span aria-hidden="true">→</span> {props.baseBranch ?? 'base branch'}</p>
            <p class="empty explorer__worktree" title={props.worktree}>{props.worktreeLabel}</p>
            <Show when={!fresh()}>
              <Show when={syncFailed()} fallback={<p class="empty" role="status">Refreshing Git status…</p>}>
                <div class="terminal-pane__bar">
                  <p class="empty" role="alert">Could not refresh Git status. Showing last known status.</p>
                  <button type="button" onClick={() => void refetch()}>Retry</button>
                </div>
              </Show>
            </Show>
            <Show when={fresh()?.mergeBlocked === 'The project root has uncommitted changes.' ? undefined : fresh()?.mergeBlocked}>{(reason) => <p class="empty">{reason()}</p>}</Show>
            <Show when={notice()}>{(message) => <p class="empty" role="status">{message()}</p>}</Show>
          </div>
          <form class="explorer__commit" data-git-commit onSubmit={(event) => {
            event.preventDefault()
            const current = fresh()
            if (!current || disabled() || !message().trim()) return
            if (commitMethod() === 'amend' && current.branch) setAmendRequested(current.stagedRevision)
            else if (commitMethod() === 'commit' && current.stagedDiff) {
              void apply({ type: 'commit', message: message(), expected: current.stagedRevision })
            }
          }}>
            <textarea ref={commitMessage} aria-label="Commit message" placeholder="Commit message" value={message()} onInput={(event) => setMessage(event.currentTarget.value)} />
            <div class="explorer__commit-footer">
              <Show when={commitMethod() === 'amend' && amendRequested() !== null && amendRequested() === fresh()?.stagedRevision} fallback={
                <div class="explorer__commit-actions" role="group" aria-label="Commit controls" data-send-accent={props.sendAccent}>
                  <button type="submit" aria-keyshortcuts={props.commitShortcut}
                    disabled={disabled() || !message().trim() || (commitMethod() === 'commit' ? !fresh()?.stagedDiff : !fresh()?.branch)}>
                    {commitMethod() === 'commit' ? 'Commit' : 'Amend'}
                  </button>
                  <PickerMenu label="Commit action" value={commitMethod()} options={[
                    { id: 'commit', name: 'Commit' }, { id: 'amend', name: 'Amend' },
                  ]} disabled={disabled()} onChange={(method) => { setCommitMethod(method === 'amend' ? 'amend' : 'commit'); setAmendRequested(null) }}
                    triggerClass="explorer__split-toggle" above align="end"><PickerCaret /></PickerMenu>
                </div>
              }>
                <span class="explorer__amend-confirmation">
                  <button type="button" disabled={disabled() || !message().trim() || !fresh()?.branch}
                    onClick={() => {
                      const current = fresh()
                      if (current && amendRequested() === current.stagedRevision) {
                        void apply({ type: 'amend', message: message(), expected: current.stagedRevision })
                      }
                    }}>
                    Replace tip on {snapshot().branch}
                  </button>
                  <button type="button" onClick={() => setAmendRequested(null)}>Cancel</button>
                </span>
              </Show>
            </div>
          </form>
        </div>
      )}</Show>
    </section>
  )
}
