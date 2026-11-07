import { For, Show, createEffect, createMemo, createResource, createSignal, onCleanup, onMount } from 'solid-js'

import { actOnWorkspaceFile, cancelFileSearch, listFiles, readWorkspaceFile, reserveFileSearch, searchWorkspaceFiles, watchWorkspaceFiles, type DirectoryListing, type FileAction, type FileChange, type FileSearchQuery, type FileSearchResult, type WorkspaceFileEntry } from './bridge'
import { FolderIcon } from './ProjectRow'

interface FileLocation {
  projectId: string
  taskId: string | null
}

interface FileSelection {
  path: string
  line: number | null
}

interface PendingSearch {
  timer?: number
  id?: string
}

export function FilesPane(props: { projectId: string; worktreeTaskId: string | null; initialFile?: { taskId: string; path: string } }) {
  const [scope, setScope] = createSignal(props.worktreeTaskId ? 'worktree' : 'project')
  const [showDotfiles, setShowDotfiles] = createSignal(false)
  const taskId = () => scope() === 'worktree' ? props.worktreeTaskId : null
  const location = createMemo(() => ({ projectId: props.projectId, taskId: taskId() }))

  return (
    <section class="files-pane" aria-label="Workspace files">
      <div class="terminal-pane__bar files-pane__toolbar">
        <select aria-label="File root" value={taskId() ? 'worktree' : 'project'}
          onChange={(event) => setScope(event.currentTarget.value)}>
          <option value="project">Project root</option>
          <Show when={props.worktreeTaskId}><option value="worktree">Task worktree</option></Show>
        </select>
        <label><input type="checkbox" checked={showDotfiles()} onChange={(event) => setShowDotfiles(event.currentTarget.checked)} /> Show dotfiles</label>
      </div>
      <Show when={location()} keyed>
        {(root) => <FileBrowser {...root} showDotfiles={showDotfiles()}
          initialPath={props.initialFile?.taskId === root.taskId ? props.initialFile?.path : undefined} />}
      </Show>
    </section>
  )
}

function FileBrowser(props: FileLocation & { showDotfiles: boolean; initialPath?: string }) {
  const [selected, setSelected] = createSignal<FileSelection | null>(props.initialPath ? { path: props.initialPath, line: null } : null)
  const [query, setQuery] = createSignal('')
  const [results, setResults] = createSignal<FileSearchResult>()
  const [error, setError] = createSignal<string>()
  const [status, setStatus] = createSignal<'idle' | 'searching' | 'cancelled'>('idle')
  const [changes, setChanges] = createSignal<FileChange[]>([])
  const [watchError, setWatchError] = createSignal<string>()
  const [treeRevision, setTreeRevision] = createSignal(0)
  const changesByPath = createMemo(() => new Map(changes().map((change) => [change.path, change])))
  const changeFor = (path: string) => changesByPath().get(path)
  const deleted = () => changes().filter((change) => change.kind === 'deleted'
    && (props.showDotfiles || !change.path.split('/').some((part) => part.startsWith('.'))))
  let active: PendingSearch | undefined

  onMount(() => {
    let closed = false
    const watcher = watchWorkspaceFiles(props.projectId, props.taskId, (update, replayed) => {
      if (closed) return
      if (update.type === 'error') {
        setWatchError(update.message)
      } else {
        setWatchError(undefined)
        setChanges(update.files)
        if (!replayed) setTreeRevision((revision) => revision + 1)
      }
    })
    void watcher.catch((failure) => { if (!closed) setWatchError(failureMessage(failure)) })
    onCleanup(() => {
      closed = true
      void watcher.then((dispose) => dispose(), () => undefined)
        .catch((failure) => console.warn('Could not stop the file watcher', failure))
    })
  })

  function stopSearch() {
    const previous = active
    active = undefined
    if (previous?.timer !== undefined) window.clearTimeout(previous.timer)
    if (previous?.id) void cancelFileSearch(previous.id).catch((failure) => console.warn('Could not cancel the file search', failure))
  }

  function search(mode: FileSearchQuery['mode']) {
    stopSearch()
    const text = query().trim()
    setResults(undefined)
    setError(undefined)
    setStatus(text ? 'searching' : 'idle')
    if (!text) return
    const request: PendingSearch = {}
    const parameters = { mode, query: text, showDotfiles: props.showDotfiles }
    active = request
    async function run() {
      try {
        const id = await reserveFileSearch()
        if (active !== request) {
          await cancelFileSearch(id)
          return
        }
        request.id = id
        const result = await searchWorkspaceFiles(id, props.projectId, props.taskId, parameters)
        if (active === request) setResults(result)
      } catch (failure) {
        if (active === request) setError(failureMessage(failure))
        else console.debug('A superseded file search ended', failure)
      } finally {
        if (active === request) {
          active = undefined
          setStatus('idle')
        }
      }
    }
    if (mode === 'name') request.timer = window.setTimeout(() => void run(), 80)
    else void run()
  }

  createEffect(() => search('name'))
  onCleanup(stopSearch)

  return (
    <>
      <form class="terminal-pane__bar files-pane__toolbar" onSubmit={(event) => {
        event.preventDefault()
        search('contents')
      }}>
        <input type="search" aria-label="Find files" placeholder="Filter by name" value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)} />
        <button type="submit" disabled={!query().trim()}>Search contents</button>
        <Show when={status() === 'searching'}>
          <button type="button" onClick={() => { stopSearch(); setStatus('cancelled') }}>Cancel search</button>
        </Show>
      </form>
      <Show when={watchError()}>{(message) => <p class="error" role="alert">{message()}</p>}</Show>
      <div class="files-pane__tree" aria-label="File tree" hidden={!!query().trim()}>
        <FolderContents projectId={props.projectId} taskId={props.taskId} path="" showDotfiles={props.showDotfiles}
          revision={treeRevision()} changeFor={changeFor} selected={selected()?.path ?? null}
          onSelect={(path) => setSelected({ path, line: null })} />
        <Show when={deleted().length}>
          <details class="files-tree__deleted" open>
            <summary>Deleted files</summary>
            <ul class="files-tree">
              <For each={deleted()}>{(file) => (
                <li class="files-tree__label"><span>{file.path}</span><FileChangeBadge change={file} /></li>
              )}</For>
            </ul>
          </details>
        </Show>
      </div>
      <Show when={query().trim()}>
        <section class="files-pane__tree" aria-label="File search results">
          <Show when={status() === 'searching'}><p class="empty empty--tight" role="status">Searching files…</p></Show>
          <Show when={status() === 'cancelled'}><p class="empty empty--tight" role="status">Search cancelled.</p></Show>
          <Show when={error()}>{(message) => <p class="error" role="alert">{message()}</p>}</Show>
          <Show when={results()}>{(result) => <>
            <Show when={result().matches.length} fallback={<p class="empty empty--tight">No matching files.</p>}>
              <ul class="files-tree">
                <For each={result().matches}>{(match) => (
                  <li>
                    <button type="button" class="files-tree__entry" onClick={() => setSelected(match)}
                      aria-pressed={selected()?.path === match.path && selected()?.line === match.line}>
                      <span class="files-tree__label">
                        <span>{match.path}{match.line === null ? '' : `:${match.line}`}</span>
                        <FileChangeBadge change={changeFor(match.path)} />
                      </span>
                      <Show when={match.excerpt}>{(excerpt) => <span class="files-search__excerpt">{excerpt()}</span>}</Show>
                    </button>
                  </li>
                )}</For>
              </ul>
            </Show>
            <Show when={result().incomplete}><p class="empty empty--tight">Some files or results were skipped.</p></Show>
          </>}</Show>
        </section>
      </Show>
      <Show when={selected()} keyed>
        {(file) => <FileViewer projectId={props.projectId} taskId={props.taskId} {...file} />}
      </Show>
    </>
  )
}

interface FolderProps extends FileLocation {
  showDotfiles: boolean
  selected: string | null
  onSelect: (path: string) => void
  revision: number
  changeFor: (path: string) => FileChange | undefined
}

function FolderContents(props: FolderProps & { path: string }) {
  const [listing] = createResource(
    () => [props.projectId, props.taskId, props.path, props.showDotfiles, props.revision] as const,
    async ([projectId, taskId, path, showDotfiles], { value }): Promise<DirectoryListing> => {
      const next = await listFiles(projectId, taskId, path, showDotfiles)
      const previous = new Map(value?.entries.map((entry) => [entry.path, entry]))
      return { ...next, entries: next.entries.map((entry) => {
        const prior = previous.get(entry.path)
        return prior && prior.directory === entry.directory ? prior : entry
      }) }
    },
  )
  return (
    <>
      <Show when={listing.loading && !listing.latest}><p class="empty empty--tight" role="status">Loading files…</p></Show>
      <Show when={listing.error}>{(error) => <p class="error" role="alert">{failureMessage(error())}</p>}</Show>
      <Show when={!listing.error && listing()}>
        {(current) => <>
          <Show when={current().entries.length} fallback={<p class="empty empty--tight">No files in this folder.</p>}>
            <ul class="files-tree">
              <For each={current().entries}>{(entry) => <FileRow {...props} entry={entry} />}</For>
            </ul>
          </Show>
          <Show when={current().incomplete}><p class="empty empty--tight">Some entries could not be shown.</p></Show>
        </>}
      </Show>
    </>
  )
}

function FileRow(props: FolderProps & { entry: WorkspaceFileEntry }) {
  const [expanded, setExpanded] = createSignal(false)
  return (
    <li>
      <Show when={props.entry.directory} fallback={
        <button type="button" class="files-tree__entry" aria-pressed={props.selected === props.entry.path}
          onClick={() => props.onSelect(props.entry.path)}>
          <span class="files-tree__label"><span>{props.entry.name}</span><FileChangeBadge change={props.changeFor(props.entry.path)} /></span>
        </button>
      }>
        <button type="button" class="files-tree__entry files-tree__directory" aria-expanded={expanded()} onClick={() => setExpanded((open) => !open)}>
          <span aria-hidden="true">{expanded() ? '▾' : '▸'}</span><FolderIcon size={14} /><span>{props.entry.name}</span>
        </button>
        <Show when={expanded()}>
          <FolderContents {...props} path={props.entry.path} />
        </Show>
      </Show>
    </li>
  )
}

const FILE_CHANGE_LABELS: Record<FileChange['kind'], string> = {
  added: 'Added', modified: 'Modified', deleted: 'Deleted', renamed: 'Renamed',
}

function FileChangeBadge(props: { change: FileChange | undefined }) {
  return (
    <Show when={props.change}>{(change) => {
      const label = () => FILE_CHANGE_LABELS[change().kind]
      const description = () => change().kind === 'renamed' && change().originalPath
        ? `Renamed from ${change().originalPath}` : label()
      return <span class="files-tree__change" data-kind={change().kind} role="img" aria-label={description()} title={description()}>{label()[0]}</span>
    }}</Show>
  )
}

function FileViewer(props: FileLocation & FileSelection) {
  const [actionPending, setActionPending] = createSignal(false)
  const [actionError, setActionError] = createSignal<string>()
  const [file] = createResource(
    () => [props.projectId, props.taskId, props.path, props.line] as const,
    ([projectId, taskId, path, line]) => readWorkspaceFile(projectId, taskId, path, line),
  )
  async function act(action: FileAction) {
    if (actionPending()) return
    setActionPending(true)
    setActionError(undefined)
    try {
      await actOnWorkspaceFile(props.projectId, props.taskId, props.path, action)
    } catch (failure) {
      setActionError(failureMessage(failure))
    } finally {
      setActionPending(false)
    }
  }

  return (
    <section class="files-viewer" aria-label="File preview">
      <div class="terminal-pane__bar"><code>{props.path}</code></div>
      <div class="terminal-pane__bar files-pane__toolbar">
        <button type="button" disabled={actionPending()} onClick={() => void act('edit')}>Open in external editor</button>
        <button type="button" disabled={actionPending()} onClick={() => void act('reveal')}>Reveal in file manager</button>
      </div>
      <Show when={actionError()}>{(message) => <p class="error" role="alert">{message()}</p>}</Show>
      <Show when={file.loading}><p class="empty empty--tight" role="status">Opening file…</p></Show>
      <Show when={file.error}>{(error) => <p class="error" role="alert">{failureMessage(error())}</p>}</Show>
      <Show when={!file.error && file()}>
        {(current) => <>
          <Show when={current().lines.length} fallback={<p class="empty empty--tight">Empty file.</p>}>
            <pre class="files-viewer__source" tabindex="0" aria-label="Read-only file contents"><code>
              <For each={current().lines}>{(line, index) => (
                <span class="files-viewer__line" aria-current={current().firstLine + index() === props.line ? 'location' : undefined}
                  ref={(row) => {
                    if (current().firstLine + index() === props.line) queueMicrotask(() => {
                      if (row.isConnected) row.scrollIntoView({ block: 'nearest' })
                    })
                  }}>
                  <span class="files-viewer__number" aria-hidden="true">{current().firstLine + index()}</span>
                  <span><For each={line}>{(part) => <span classList={{ 'files-viewer__syntax': !!part.color }}
                    style={{ '--syntax-color': part.color ?? undefined }}>{part.text}</span>}</For></span>
                </span>
              )}</For>
            </code></pre>
          </Show>
          <Show when={current().truncated}><p class="empty empty--tight">Showing lines {current().firstLine}–{current().firstLine + current().lines.length - 1}.</p></Show>
        </>}
      </Show>
    </section>
  )
}

function failureMessage(error: unknown): string {
  return (error as { message?: string } | null)?.message ?? String(error)
}
