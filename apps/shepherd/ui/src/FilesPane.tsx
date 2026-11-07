import { For, Show, createEffect, createMemo, createResource, createSignal, onCleanup, onMount } from 'solid-js'

import { actOnWorkspaceFile, cancelFileSearch, listFiles, readWorkspaceFile, reserveFileSearch, searchWorkspaceFiles, watchWorkspaceFiles, type DirectoryListing, type FileAction, type FileChange, type FileSearchQuery, type FileSearchResult, type WorkspaceFileEntry } from './bridge'
import iconTheme from './charmed-icons.json'

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

interface CachedDirectory {
  promise: Promise<DirectoryListing>
  value?: DirectoryListing
}

export function FilesPane(props: { projectId: string; worktreeTaskId: string | null; initialFile?: { taskId: string; path: string } }) {
  const [scope, setScope] = createSignal(props.worktreeTaskId ? 'worktree' : 'project')
  const [showDotfiles, setShowDotfiles] = createSignal(false)
  const taskId = () => scope() === 'worktree' ? props.worktreeTaskId : null
  const location = createMemo(() => ({ projectId: props.projectId, taskId: taskId() }))

  return (
    <section class="files-pane" aria-label="Workspace files">
      <div class="terminal-pane__bar files-pane__toolbar">
        <span class="files-root-picker">
          <select aria-label="File root" value={taskId() ? 'worktree' : 'project'}
            onChange={(event) => setScope(event.currentTarget.value)}>
            <option value="project">Project root</option>
            <Show when={props.worktreeTaskId}><option value="worktree">Task worktree</option></Show>
          </select>
          <svg aria-hidden="true" width="10" height="10" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="m2.5 4.5 3.5 3.5 3.5-3.5" /></svg>
        </span>
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
  const [openPath, setOpenPath] = createSignal(props.initialPath ? parentPath(props.initialPath) : '')
  const [query, setQuery] = createSignal('')
  const [results, setResults] = createSignal<FileSearchResult>()
  const [error, setError] = createSignal<string>()
  const [status, setStatus] = createSignal<'idle' | 'searching' | 'cancelled'>('idle')
  const [changes, setChanges] = createSignal<FileChange[]>([])
  const [watchError, setWatchError] = createSignal<string>()
  const [treeRevision, setTreeRevision] = createSignal(0)
  const directories = new Map<string, CachedDirectory>()
  let cacheVersion = ''
  let navigation = 0
  let disposed = false
  onCleanup(() => { disposed = true })

  function loadDirectory(path: string, showDotfiles: boolean, revision: number): Promise<DirectoryListing> {
    const version = `${showDotfiles}:${revision}`
    if (cacheVersion !== version) { directories.clear(); cacheVersion = version }
    const cached = directories.get(path)
    if (cached) return cached.promise
    const entry: CachedDirectory = { promise: listFiles(props.projectId, props.taskId, path, showDotfiles) }
    directories.set(path, entry)
    entry.promise.then((value) => { entry.value = value }, () => {
      if (directories.get(path) === entry) directories.delete(path)
    })
    return entry.promise
  }

  function cachedDirectory(path: string, showDotfiles: boolean, revision: number): DirectoryListing | undefined {
    return cacheVersion === `${showDotfiles}:${revision}` ? directories.get(path)?.value : undefined
  }

  function preloadDirectories(listing: DirectoryListing, showDotfiles: boolean, revision: number) {
    const queue = listing.entries.filter((entry) => entry.directory).map((entry) => ({ path: entry.path, depth: 1 }))
    let next = 0
    async function worker() {
      while (!disposed && showDotfiles === props.showDotfiles && revision === treeRevision() && next < queue.length && next < 128) {
        const item = queue[next++]
        try {
          const child = await loadDirectory(item.path, showDotfiles, revision)
          if (item.depth < 2) queue.push(...child.entries.filter((entry) => entry.directory).map((entry) => ({ path: entry.path, depth: item.depth + 1 })))
        } catch (error) {
          console.debug('Could not preload folder', item.path, error)
        }
      }
    }
    for (let index = 0; index < Math.min(4, queue.length); index++) void worker()
  }

  async function toggleFolder(path: string) {
    const request = ++navigation
    const current = openPath()
    if (current === path || current.startsWith(`${path}/`)) {
      setOpenPath(parentPath(path))
      return
    }
    try {
      await loadDirectory(path, props.showDotfiles, treeRevision())
    } catch (error) {
      console.debug('Could not open folder', path, error)
    }
    if (!disposed && request === navigation) setOpenPath(path)
  }
  const changesByPath = createMemo(() => new Map(changes().map((change) => [change.path, change])))
  const changeFor = (path: string) => changesByPath().get(path)
  const directoryChangeFor = (path: string) => changes().find((change) => change.path.startsWith(`${path}/`))
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
          revision={treeRevision()} changeFor={changeFor} directoryChangeFor={directoryChangeFor} selected={selected()?.path ?? null}
          loadDirectory={loadDirectory} cachedDirectory={cachedDirectory} preloadDirectories={preloadDirectories}
          openPath={openPath()} onToggleFolder={(path) => void toggleFolder(path)}
          onSelect={(path) => { navigation++; setOpenPath(parentPath(path)); setSelected({ path, line: null }) }} />
        <Show when={deleted().length}>
          <details class="files-tree__deleted" open>
            <summary>Deleted files</summary>
            <ul class="files-tree">
              <For each={deleted()}>{(file) => (
                <li class="files-tree__label files-tree__deleted-row" data-kind={file.kind}><FileIcon name={file.path} /><span>{file.path}</span><FileChangeBadge change={file} /></li>
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
                    <button type="button" class="files-tree__entry" data-kind={changeFor(match.path)?.kind} onClick={() => { setOpenPath(parentPath(match.path)); setSelected(match) }}
                      aria-pressed={selected()?.path === match.path && selected()?.line === match.line}>
                      <span class="files-tree__label">
                        <span class="files-tree__name"><FileIcon name={match.path} /><span>{match.path}{match.line === null ? '' : `:${match.line}`}</span></span>
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
  openPath: string
  onToggleFolder: (path: string) => void
  revision: number
  changeFor: (path: string) => FileChange | undefined
  directoryChangeFor: (path: string) => FileChange | undefined
  loadDirectory: (path: string, showDotfiles: boolean, revision: number) => Promise<DirectoryListing>
  cachedDirectory: (path: string, showDotfiles: boolean, revision: number) => DirectoryListing | undefined
  preloadDirectories: (listing: DirectoryListing, showDotfiles: boolean, revision: number) => void
}

function FolderContents(props: FolderProps & { path: string }) {
  const [listing] = createResource(
    () => [props.projectId, props.taskId, props.path, props.showDotfiles, props.revision] as const,
    async ([, , path, showDotfiles, revision], { value }): Promise<DirectoryListing> => {
      const next = await props.loadDirectory(path, showDotfiles, revision)
      props.preloadDirectories(next, showDotfiles, revision)
      const previous = new Map(value?.entries.map((entry) => [entry.path, entry]))
      return { ...next, entries: next.entries.map((entry) => {
        const prior = previous.get(entry.path)
        return prior && prior.directory === entry.directory && prior.ignored === entry.ignored ? prior : entry
      }) }
    },
  )
  return (
    <>
      <Show when={listing.loading && !listing.latest && !props.cachedDirectory(props.path, props.showDotfiles, props.revision)}><p class="empty empty--tight" role="status">Loading files…</p></Show>
      <Show when={listing.error}>{(error) => <p class="error" role="alert">{failureMessage(error())}</p>}</Show>
      <Show when={!listing.error && (listing.latest ?? props.cachedDirectory(props.path, props.showDotfiles, props.revision))}>
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
  const expanded = () => props.openPath === props.entry.path || props.openPath.startsWith(`${props.entry.path}/`)
  return (
    <li>
      <Show when={props.entry.directory} fallback={
        <button type="button" class="files-tree__entry" classList={{ 'files-tree__entry--ignored': props.entry.ignored }} data-kind={props.changeFor(props.entry.path)?.kind} aria-pressed={props.selected === props.entry.path}
          onClick={() => props.onSelect(props.entry.path)}>
          <span class="files-tree__label"><span class="files-tree__name"><FileIcon name={props.entry.name} ignored={props.entry.ignored} /><span>{props.entry.name}</span></span><FileChangeBadge change={props.changeFor(props.entry.path)} /></span>
        </button>
      }>
        <button type="button" class="files-tree__entry files-tree__directory" classList={{ 'files-tree__entry--ignored': props.entry.ignored }} data-kind={props.directoryChangeFor(props.entry.path)?.kind} aria-expanded={expanded()} onClick={() => props.onToggleFolder(props.entry.path)}>
          <ThemeIcon icon={expanded() ? '_folder_open' : '_folder'} ignored={props.entry.ignored} /><span>{props.entry.name}</span>
        </button>
        <Show when={expanded()}>
          <FolderContents {...props} path={props.entry.path} />
        </Show>
      </Show>
    </li>
  )
}

function parentPath(path: string): string {
  const separator = path.lastIndexOf('/')
  return separator < 0 ? '' : path.slice(0, separator)
}

function FileIcon(props: { name: string; ignored?: boolean }) {
  const icon = () => {
    const name = (props.name.split('/').pop() ?? '').toLowerCase()
    const stems: Record<string, string> = iconTheme.stems
    const suffixes: Record<string, string> = iconTheme.suffixes
    if (Object.hasOwn(stems, name)) return stems[name]
    for (let index = name.indexOf('.'); index >= 0; index = name.indexOf('.', index + 1)) {
      const suffix = name.slice(index + 1)
      if (Object.hasOwn(suffixes, suffix)) return suffixes[suffix]
    }
    return '_file'
  }
  return <ThemeIcon icon={icon()} ignored={props.ignored} />
}

function ThemeIcon(props: { icon: string; ignored?: boolean }) {
  const url = () => `${import.meta.env.BASE_URL}charmed-icons/${props.icon}.svg`
  return <Show when={props.ignored} fallback={<img class="files-tree__icon" src={url()} alt="" />}>
    <span class="files-tree__icon files-tree__icon--ignored" aria-hidden="true" style={{ 'mask-image': `url("${url()}")` }} />
  </Show>
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
