import { For, Show, createMemo, createSignal, createUniqueId } from 'solid-js'
import type { GitFile, GitSnapshot } from './bridge'
import { FileIcon, ThemeIcon } from './FilesPane'
import { describeStatus } from './GitDiff'

type LineStats = GitSnapshot['lineStats'][number]
interface TreeNode {
  name: string
  path: string
  children: Map<string, TreeNode>
  file?: GitFile
}

function kind(file: GitFile) {
  const status = `${file.index}${file.worktree}`
  if (status.includes('R')) return 'renamed'
  if (status.includes('D')) return 'deleted'
  if (status.includes('A') || status === '??') return 'added'
  return 'modified'
}

export function ChangedFiles(props: {
  files: GitFile[]
  stats: LineStats[]
  selected: string | null
  onSelect: (path: string) => void
}) {
  const stats = createMemo(() => new Map(props.stats.map((entry) => [entry.path, entry])))
  const totals = createMemo(() => props.files.reduce((sum, file) => {
    const entry = stats().get(file.path)
    if (!entry || entry.added === null || entry.removed === null) sum.unknown = true
    else { sum.added += entry.added; sum.removed += entry.removed }
    return sum
  }, { added: 0, removed: 0, unknown: false }))
  const tree = createMemo(() => {
    const root = new Map<string, TreeNode>()
    for (const file of props.files) {
      const parts = file.path.split('/')
      let nodes = root
      let path = ''
      for (const [index, name] of parts.entries()) {
        path = path ? `${path}/${name}` : name
        let node = nodes.get(name)
        if (!node) { node = { name, path, children: new Map() }; nodes.set(name, node) }
        if (index === parts.length - 1) node.file = file
        nodes = node.children
      }
    }
    return root
  })
  return <section class="changed-files" aria-label="Changed files">
    <div class="changed-files__summary">
      <span>{props.files.length} {props.files.length === 1 ? 'changed file' : 'changed files'}</span>
      <LineCounts added={totals().added} removed={totals().removed} />
    </div>
    <Show when={totals().unknown}><p class="empty">Some files have binary or unavailable line counts.</p></Show>
    <Show when={props.files.length} fallback={<p class="empty">Working tree clean.</p>}>
      <ChangedNodes nodes={tree()} stats={stats()} selected={props.selected} onSelect={props.onSelect} />
    </Show>
  </section>
}

function ChangedNodes(props: { nodes: Map<string, TreeNode>; stats: Map<string, LineStats>; selected: string | null; onSelect: (path: string) => void }) {
  const nodes = () => [...props.nodes.values()].sort((left, right) => Number(right.children.size > 0) - Number(left.children.size > 0) || left.name.localeCompare(right.name))
  return <ul class="files-tree"><For each={nodes()}>{(node) => <ChangedNode node={node} stats={props.stats} selected={props.selected} onSelect={props.onSelect} />}</For></ul>
}

function ChangedNode(props: { node: TreeNode; stats: Map<string, LineStats>; selected: string | null; onSelect: (path: string) => void }) {
  const [expanded, setExpanded] = createSignal(true)
  const statusId = createUniqueId()
  const file = () => props.node.file
  return <li>
    <Show when={props.node.children.size > 0} fallback={<Show when={file()}>{(current) => {
      const counts = () => props.stats.get(current().path)
      return <>
        <button type="button" class="files-tree__entry" data-kind={kind(current())}
          aria-label={current().path} aria-describedby={statusId} aria-pressed={props.selected === current().path}
          disabled={!current().actionable} onClick={() => props.onSelect(current().path)}>
          <span class="files-tree__label"><span class="files-tree__name"><FileIcon name={props.node.name} /><span>{props.node.name}</span></span>
            <Show when={counts()} fallback={<span class="changed-files__unknown">—</span>}>
              {(entry) => <Show when={entry().added !== null && entry().removed !== null} fallback={<span class="changed-files__unknown">Binary</span>}>
                <LineCounts added={entry().added ?? 0} removed={entry().removed ?? 0} />
              </Show>}
            </Show>
          </span>
        </button>
        <span id={statusId} hidden>{describeStatus(current().index.trim(), current().worktree.trim())}{counts()?.added !== null && counts()?.removed !== null && counts() ? `. ${counts()?.added} lines added, ${counts()?.removed} lines removed` : ''}</span>
      </>
    }}</Show>}>
      <button type="button" class="files-tree__entry files-tree__directory" aria-expanded={expanded()} onClick={() => setExpanded(!expanded())}>
        <ThemeIcon icon={expanded() ? '_folder_open' : '_folder'} /><span>{props.node.name}</span>
      </button>
      <Show when={expanded()}><ChangedNodes nodes={props.node.children} stats={props.stats} selected={props.selected} onSelect={props.onSelect} /></Show>
    </Show>
  </li>
}

function LineCounts(props: { added: number; removed: number }) {
  return <span class="changed-files__counts" role="img" aria-label={`${props.added} lines added, ${props.removed} lines removed`}>
    <span class="changed-files__added">+{props.added}</span><span class="changed-files__removed">−{props.removed}</span>
  </span>
}
