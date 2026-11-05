import { Channel, invoke } from '@tauri-apps/api/core'

/**
 * The IPC contract. These types mirror the core's snapshots exactly; the
 * frontend never derives its own task state.
 */

export type TaskState =
  | 'draft'
  | 'starting'
  | 'ready'
  | 'working'
  | 'waiting'
  | 'queued'
  | 'review'
  | 'done'
  | 'failed'
  | 'cancelled'
  | 'interrupted'

export type AgentTier = 'native' | 'acp' | 'terminal-wrapper' | 'plain-pane'

export type ShutdownState = 'running' | 'draining' | 'terminating' | 'closed'

export type Decision = 'allow' | 'deny'

export interface ProjectSnapshot {
  id: string
  path: string
  name: string
  missing: boolean
  lastTaskId: string | null
  unfinishedTaskCount: number
  reviewTaskCount: number
}

export interface ProjectList {
  projects: ProjectSnapshot[]
}

/** One row in the project picker: a repository, a directory leading to one, or
 * a synthetic group holding the entries a level could not show. A group has an
 * empty path. */
export interface ProjectNode {
  name: string
  path: string
  repository: boolean
  children: ProjectNode[]
}

export interface ProjectScan {
  root: string
  repositories: number
  tree: ProjectNode[]
}

export interface TaskSummary {
  id: string
  projectId: string
  title: string
  state: TaskState
  tier: AgentTier
  branch: string | null
  worktreePath: string | null
  error: string | null
  createdAt: number
}

export interface TaskList {
  projectId: string
  tasks: TaskSummary[]
}

export interface TaskDetail {
  id: string
  projectId: string
  title: string
  state: TaskState
  tier: AgentTier
  proposedBranch: string
  proposedWorktreePath: string
  branch: string | null
  worktreePath: string | null
  baseBranch: string | null
  error: string | null
  createdAt: number
  updatedAt: number
}

export interface FileDiff {
  path: string
  oldText: string | null
  newText: string
}

export interface PlanRow {
  text: string
  status: string
}

export interface ToolCallEntry {
  toolCallId: string
  title: string
  kind: string
  status: 'pending' | 'running' | 'completed' | 'failed'
  output: string | null
  diffs: FileDiff[]
  terminalId: string | null
}

export interface PermissionEntry {
  approvalId: string
  title: string
  arguments: unknown
  paths: string[]
  patch: FileDiff | null
  decision: Decision | null
}

export type FailureKind =
  | 'missingBinary'
  | 'unsupportedVersion'
  | 'authRequired'
  | 'protocol'
  | 'midTurnExit'

export type EntryKind =
  | { type: 'userMessage'; data: { text: string } }
  | { type: 'agentText'; data: { text: string } }
  | { type: 'thought'; data: { text: string } }
  | { type: 'toolCall'; data: ToolCallEntry }
  | { type: 'plan'; data: { rows: PlanRow[] } }
  | { type: 'permission'; data: PermissionEntry }
  | { type: 'notice'; data: { text: string; isError: boolean } }
  | { type: 'failure'; data: { kind: FailureKind; text: string; detail: string | null } }
  | { type: 'usage'; data: { used: number; size: number; cost: number | null } }

export interface Entry {
  id: string
  sequence: number
  kind: EntryKind
}

export interface AgentDiagnostics {
  command: string
  protocol: string
  stderrTail: string
}

export interface TaskWorkspace {
  queuedReviewNote?: string
  project: ProjectSnapshot
  task: TaskDetail
  transcript: Entry[]
  diagnostics: AgentDiagnostics | null
}

export interface QuitSummary {
  interruptedTasks: number
  liveTerminals: number
}

export interface TerminalSnapshot {
  id: string
  projectId: string
  taskId: string | null
  scope: 'project' | 'task'
  title: string
  cwd: string
  command: string
  createdAt: number
  endedAt: number | null
  exitCode: number | null
}

export interface FrameCell {
  text: string
  width: number
  foreground: number
  background: number
  bold: boolean
  italic: boolean
  linked: boolean
}

export interface TerminalFrame {
  columns: number
  rows: number
  cursorColumn: number
  cursorRow: number
  cursorVisible: boolean
  displayOffset: number
  modes: number
  cells: FrameCell[]
}

export type Event =
  | { type: 'projectListChanged'; data: ProjectList }
  | { type: 'terminalListChanged'; data: { projectId: string; terminals: TerminalSnapshot[] } }
  | { type: 'taskListChanged'; data: TaskList }
  | { type: 'taskChanged'; data: TaskSummary }
  | { type: 'taskWorkspaceChanged'; data: TaskWorkspace }
  | { type: 'shutdownStateChanged'; data: ShutdownState }
  | { type: 'error'; data: { taskId: string | null; message: string } }

export interface BridgeError {
  code: string
  message: string
}

export function addProject(path: string): Promise<ProjectList> {
  return invoke<ProjectList>('add_project', { path })
}

export function createDraft(projectId: string, title: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('create_draft', { projectId, title })
}

export function discardDraft(taskId: string): Promise<void> {
  return invoke<void>('discard_draft', { taskId })
}

export function sendPrompt(taskId: string, text: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('send_prompt', { taskId, text })
}

export function resolveApproval(
  taskId: string,
  approvalId: string,
  decision: Decision,
): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('resolve_approval', { taskId, approvalId, decision })
}

export function cancelTask(taskId: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('cancel_task', { taskId })
}

export function resumeTask(taskId: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('resume_task', { taskId })
}

export function quit(force: boolean): Promise<QuitSummary> {
  return invoke<QuitSummary>('quit', { force })
}

export function projectList(): Promise<ProjectList> {
  return invoke<ProjectList>('project_list')
}

export function scanProjects(rescan: boolean): Promise<ProjectScan> {
  return invoke<ProjectScan>('project_scan', { rescan })
}

export function taskList(projectId: string): Promise<TaskList> {
  return invoke<TaskList>('task_list', { projectId })
}

export function taskWorkspace(taskId: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('task_workspace', { taskId })
}

export type SplitAxis = 'horizontal' | 'vertical'
export type TerminalSplit =
  | { type: 'pane'; terminalId: string }
  | { type: 'split'; axis: SplitAxis; ratio: number; first: TerminalSplit; second: TerminalSplit }
export interface TerminalLayout {
  revision: number
  root: TerminalSplit | null
}

export function terminalLayout(projectId: string): Promise<TerminalLayout> {
  return invoke('terminal_layout', { projectId })
}

export function saveTerminalLayout(projectId: string, layout: TerminalLayout): Promise<TerminalLayout> {
  return invoke('save_terminal_layout', { projectId, layout })
}

export function terminalList(projectId: string): Promise<TerminalSnapshot[]> {
  return invoke<TerminalSnapshot[]>('terminal_list', { projectId })
}

export function startTerminal(
  projectId: string,
  taskId: string | null,
  columns: number,
  rows: number,
): Promise<TerminalSnapshot[]> {
  return invoke<TerminalSnapshot[]>('start_terminal', { projectId, taskId, columns, rows })
}

export function restartTerminal(terminalId: string, columns: number, rows: number): Promise<TerminalSnapshot[]> {
  return invoke('restart_terminal', { terminalId, columns, rows })
}

interface SearchPoint {
  line: number
  column: number
}

export interface TerminalMatch {
  start: SearchPoint
  end: SearchPoint
}

export function searchTerminal(terminalId: string, query: string, before: SearchPoint | null): Promise<TerminalMatch | null> {
  return invoke('search_terminal', { terminalId, query, before })
}

export function terminalScroll(terminalId: string, lines: number): Promise<void> {
  return invoke('terminal_scroll', { terminalId, lines })
}

export function terminalWrite(terminalId: string, data: string | number[]): Promise<void> {
  return invoke<void>('terminal_write', { terminalId, data })
}

export function openTerminalLink(terminalId: string, row: number, column: number): Promise<void> {
  return invoke('open_terminal_link', { terminalId, row, column })
}

export function terminalResize(
  terminalId: string,
  columns: number,
  rows: number,
): Promise<void> {
  return invoke<void>('terminal_resize', { terminalId, columns, rows })
}

export function closeTerminal(terminalId: string): Promise<void> {
  return invoke<void>('close_terminal', { terminalId })
}

function decodeTerminalFrame(buffer: ArrayBuffer, previous: TerminalFrame | undefined) {
  const view = new DataView(buffer)
  let offset = 0
  const byte = () => view.getUint8(offset++)
  const short = () => { const value = view.getUint16(offset, true); offset += 2; return value }
  const integer = () => { const value = view.getUint32(offset, true); offset += 4; return value }

  if (byte() !== 1) throw new Error('Unsupported terminal frame version')
  const flags = byte()
  const columns = short()
  const rows = short()
  const cursorColumn = short()
  const cursorRow = short()
  const displayOffset = integer()
  const modes = integer()
  const rowCount = short()
  const full = Boolean(flags & 2)
  if (columns < 2 || columns > 512 || !rows || rows > 256 || rowCount > rows || cursorRow >= rows || cursorColumn >= columns) {
    throw new Error('Invalid terminal frame dimensions')
  }
  if (full ? rowCount !== rows : previous?.columns !== columns || previous.rows !== rows || previous.displayOffset !== displayOffset) {
    throw new Error('Terminal frame is missing its base')
  }

  const decoder = new TextDecoder()
  const cells = full ? new Array<FrameCell>(columns * rows) : previous!.cells.slice()
  const changedRows: number[] = []
  let previousRow = -1
  for (let row = 0; row < rowCount; row++) {
    const rowIndex = short()
    const runCount = short()
    if (rowIndex <= previousRow || rowIndex >= rows) throw new Error('Invalid terminal row order')
    changedRows.push(rowIndex)
    previousRow = rowIndex
    let column = 0
    for (let run = 0; run < runCount; run++) {
      const length = short()
      const attributes = byte()
      const foreground = integer()
      const background = integer()
      const textLength = integer()
      if (!length || column + length > columns) throw new Error('Invalid terminal cell run')
      const cell = {
        text: decoder.decode(new Uint8Array(buffer, offset, textLength)),
        width: attributes & 3,
        foreground,
        background,
        bold: Boolean(attributes & 4),
        italic: Boolean(attributes & 8),
        linked: Boolean(attributes & 16),
      }
      offset += textLength
      cells.fill(cell, rowIndex * columns + column, rowIndex * columns + column + length)
      column += length
    }
    if (column !== columns) throw new Error('Incomplete terminal row')
  }
  if (offset !== buffer.byteLength) throw new Error('Invalid terminal frame length')
  const frame: TerminalFrame = { columns, rows, cursorColumn, cursorRow, cursorVisible: Boolean(flags & 1), displayOffset, modes, cells }
  return { frame, changedRows: full ? null : changedRows }
}

export async function subscribeTerminal(
  terminalId: string,
  handler: (frame: TerminalFrame, changedRows: number[] | null) => void,
  onError: (failure: unknown) => void,
): Promise<() => Promise<void>> {
  let previous: TerminalFrame | undefined
  const channel = new Channel<ArrayBuffer>((buffer) => {
    try {
      const update = decodeTerminalFrame(buffer, previous)
      previous = update.frame
      handler(update.frame, update.changedRows)
    } catch (failure) {
      onError(failure)
    }
  })
  const subscriptionId = await invoke<string>('subscribe_terminal', { terminalId, channel })
  return () => invoke<void>('unsubscribe_terminal', { subscriptionId })
}

/** Subscribe the window to the core's single event stream. */
export async function subscribeToEvents(handler: (event: Event) => void): Promise<Channel<Event>> {
  const channel = new Channel<Event>()
  channel.onmessage = handler
  await invoke('subscribe', { channel })
  return channel
}

export interface GitFile {
  path: string
  originalPath: string | null
  actionable: boolean
  index: string
  worktree: string
}

export interface ReviewSnapshot {
  files: GitFile[]
}

export function reviewFileDiff(taskId: string, file: string): Promise<string> {
  return invoke('review_file_diff', { taskId, file })
}

export function reviewSnapshot(taskId: string): Promise<ReviewSnapshot> {
  return invoke('review_snapshot', { taskId })
}

export interface GitSnapshot {
  branch: string | null
  tracking: string | null
  projectRoot: { branch: string | null; tracking: string | null; files: GitFile[] } | null
  branches: string[]
  history: { id: string; author: string; time: string; subject: string }[]
  files: GitFile[]
  stagedRevision: string
  mergeBlocked: string | null
  stagedDiff: string
  unstagedDiff: string
}

export function gitSnapshot(taskId: string): Promise<GitSnapshot> {
  return invoke('git_snapshot', { taskId })
}

export interface GitFileDiff {
  staged: string
  unstaged: string
  revision: string
  canDiscard: boolean
}

export function gitFileDiff(taskId: string, file: string): Promise<GitFileDiff> {
  return invoke('git_file_diff', { taskId, file })
}

export type GitMutation =
  | { type: 'stage'; path: string | null }
  | { type: 'unstage'; path: string }
  | { type: 'commit'; message: string; expected: string }
  | { type: 'amend'; message: string; expected: string }
  | { type: 'discard'; path: string; expected: string }
  | { type: 'checkout'; branch: string }
  | { type: 'createBranch'; branch: string }
  | { type: 'squashMerge' }

export function mutateGit(taskId: string, mutation: GitMutation): Promise<GitSnapshot> {
  return invoke('mutate_git', { taskId, mutation })
}

export type ReviewAction =
  | { type: 'archive' }
  | { type: 'reopen' }
  | { type: 'sendNote'; text: string }
  | { type: 'sendQueuedNote' }
  | { type: 'removeQueuedNote' }

export function reviewTask(taskId: string, action: ReviewAction): Promise<TaskWorkspace> {
  return invoke('review_task', { taskId, action })
}
