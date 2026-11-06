import { For, Show, createMemo, createResource, createSignal } from 'solid-js'

import { listFiles, readWorkspaceFile, type WorkspaceFileEntry } from './bridge'

interface FileLocation {
  projectId: string
  taskId: string | null
}

export function FilesPane(props: { projectId: string; worktreeTaskId: string | null }) {
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
        {(root) => <FileBrowser {...root} showDotfiles={showDotfiles()} />}
      </Show>
    </section>
  )
}

function FileBrowser(props: FileLocation & { showDotfiles: boolean }) {
  const [selected, setSelected] = createSignal<string | null>(null)
  return (
    <>
      <div class="files-pane__tree" aria-label="File tree">
        <FolderContents projectId={props.projectId} taskId={props.taskId} path="" showDotfiles={props.showDotfiles}
          selected={selected()} onSelect={setSelected} />
      </div>
      <Show when={selected()} keyed>
        {(path) => <FileViewer projectId={props.projectId} taskId={props.taskId} path={path} />}
      </Show>
    </>
  )
}

interface FolderProps extends FileLocation {
  showDotfiles: boolean
  selected: string | null
  onSelect: (path: string) => void
}

function FolderContents(props: FolderProps & { path: string }) {
  const [listing] = createResource(
    () => [props.projectId, props.taskId, props.path, props.showDotfiles] as const,
    ([projectId, taskId, path, showDotfiles]) => listFiles(projectId, taskId, path, showDotfiles),
  )
  return (
    <>
      <Show when={listing.loading}><p class="empty empty--tight" role="status">Loading files…</p></Show>
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
          onClick={() => props.onSelect(props.entry.path)}>{props.entry.name}</button>
      }>
        <button type="button" class="files-tree__entry" aria-expanded={expanded()} onClick={() => setExpanded((open) => !open)}>
          <span aria-hidden="true">{expanded() ? '▾' : '▸'}</span> {props.entry.name}
        </button>
        <Show when={expanded()}>
          <FolderContents {...props} path={props.entry.path} />
        </Show>
      </Show>
    </li>
  )
}

function FileViewer(props: FileLocation & { path: string }) {
  const [file] = createResource(
    () => [props.projectId, props.taskId, props.path] as const,
    ([projectId, taskId, path]) => readWorkspaceFile(projectId, taskId, path),
  )
  return (
    <section class="files-viewer" aria-label="File preview">
      <div class="terminal-pane__bar"><code>{props.path}</code></div>
      <Show when={file.loading}><p class="empty empty--tight" role="status">Opening file…</p></Show>
      <Show when={file.error}>{(error) => <p class="error" role="alert">{failureMessage(error())}</p>}</Show>
      <Show when={!file.error && file()}>
        {(current) => <>
          <Show when={current().lines.length} fallback={<p class="empty empty--tight">Empty file.</p>}>
            <pre class="files-viewer__source" tabindex="0" aria-label="Read-only file contents"><code>
              <For each={current().lines}>{(line, index) => (
                <span class="files-viewer__line">
                  <span class="files-viewer__number" aria-hidden="true">{index() + 1}</span>
                  <span><For each={line}>{(part) => <span style={{ color: part.color ?? undefined }}>{part.text}</span>}</For></span>
                </span>
              )}</For>
            </code></pre>
          </Show>
          <Show when={current().truncated}><p class="empty empty--tight">The preview shows the first 4,000 lines.</p></Show>
        </>}
      </Show>
    </section>
  )
}

function failureMessage(error: unknown): string {
  return (error as { message?: string } | null)?.message ?? String(error)
}
