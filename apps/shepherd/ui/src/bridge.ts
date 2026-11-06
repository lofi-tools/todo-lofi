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
export type Agent = 'codex' | 'opencode' | 'claude'
export const AGENT_LABELS: Record<Agent, string> = { codex: 'Codex', opencode: 'OpenCode', claude: 'Claude Code' }

export type ShutdownState = 'running' | 'draining' | 'terminating' | 'closed'

export type Decision = 'allow' | 'allowSession' | 'allowProject' | 'deny'

export interface ProjectSnapshot {
  id: string
  path: string
  name: string
  missing: boolean
  isGitRepository: boolean
  lastTaskId: string | null
  unfinishedTaskCount: number
  reviewTaskCount: number
}

export interface ProjectList {
  projects: ProjectSnapshot[]
}

export interface ProjectRemote {
  label: string
  forge: 'github' | 'gitlab' | 'huggingface' | null
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

export interface LocalCodexProject {
  name: string
  path: string
}

export interface TaskSummary {
  id: string
  projectId: string
  title: string
  state: TaskState
  agent: Agent
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
  agent: Agent
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
  status: 'pending' | 'running' | 'completed' | 'failed' | 'stopped'
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
  choices?: Decision[]
  source?: 'user' | 'agent' | 'sessionGrant' | 'projectGrant'
  withdrawn: boolean
}

export type FailureKind =
  | 'missingBinary'
  | 'unsupportedVersion'
  | 'authRequired'
  | 'agentRejected'
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
  fallbackReason?: string
  model?: string
  stderrTail: string
}

export interface AgentCapabilities {
  streaming: boolean
  thoughts: boolean
  toolCalls: boolean
  approvals: boolean
  resume: boolean
  cancel: boolean
  modelPicker: boolean
  modePicker: boolean
  images: boolean
  resourceLinks: boolean
  usage: boolean
  agentTerminals: boolean
  patchesInApprovals: boolean
}

export type SessionRecovery = 'resume' | 'reconnect' | 'startNew'

export interface WorkspaceLayout {
  inspectorWidth: number | null
}

export interface TaskWorkspace {
  cancelling: boolean
  queuedReviewNote?: string
  project: ProjectSnapshot
  task: TaskDetail
  layout: WorkspaceLayout
  transcript: Entry[]
  agentTerminals: AgentTerminal[]
  diagnostics: AgentDiagnostics | null
  adapterTier: AgentTier | null
  capabilities: AgentCapabilities | null
  recovery: SessionRecovery | null
}

export interface AgentTerminal {
  id: string
  title: string
  live: boolean
  exitCode: number | null
}

export interface QuitSummary {
  interruptedTasks: number
  liveTerminals: number
}

export interface QuitConfirmation {
  liveTasks: number
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

export interface TerminalList {
  projectId: string
  revision: number
  terminals: TerminalSnapshot[]
}

export interface FrameCell {
  text: string
  width: number
  foreground: number
  background: number
  bold: boolean
  italic: boolean
  linked: boolean
  wraps: boolean
  underline: boolean
  strikeout: boolean
}

export interface TerminalFrame {
  revision: number
  columns: number
  rows: number
  cursorColumn: number
  cursorRow: number
  cursorVisible: boolean
  cursorStyle: number
  displayOffset: number
  modes: number
  cells: FrameCell[]
  searchHighlight: { id: number; start: number; end: number } | null
}

export type Event =
  | { type: 'projectListChanged'; data: ProjectList }
  | { type: 'projectApprovalsChanged'; data: { projectId: string } }
  | { type: 'terminalBell'; data: { projectId: string; taskId: string | null; terminalId: string } }
  | { type: 'terminalListChanged'; data: TerminalList }
  | { type: 'taskListChanged'; data: TaskList }
  | { type: 'taskChanged'; data: TaskSummary }
  | { type: 'taskWorkspaceChanged'; data: TaskWorkspace }
  | { type: 'shutdownStateChanged'; data: ShutdownState }
  | { type: 'error'; data: { taskId: string | null; message: string } }

export interface BridgeError {
  code: string
  message: string
  quitConfirmation?: QuitConfirmation
}

export function addProject(path: string): Promise<ProjectList> {
  return invoke<ProjectList>('add_project', { path })
}

export function createDraft(projectId: string, title: string, agent: Agent = 'codex'): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('create_draft', { projectId, title, agent })
}

export function setDraftAgent(taskId: string, agent: Agent): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('set_draft_agent', { taskId, agent })
}

export function opencodeConfigured(): Promise<boolean> {
  return invoke<boolean>('opencode_configured')
}

export function claudeConfigured(): Promise<boolean> {
  return invoke<boolean>('claude_configured')
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

export interface ProjectApproval {
  id: string
  agent: Agent
  title: string
  arguments: unknown
  paths: string[]
}

export interface SessionApproval extends ProjectApproval {
  taskId: string
  taskTitle: string
}

export interface ProjectApprovalsSnapshot {
  saved: ProjectApproval[]
  sessions: SessionApproval[]
}

export function projectApprovals(projectId: string) {
  return invoke<ProjectApprovalsSnapshot>('project_approvals', { projectId })
}

export function resetApprovalGrant(projectId: string, approvalId: string, taskId?: string) {
  return invoke<void>('reset_approval_grant', { projectId, approvalId, taskId })
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

export function projectRemote(projectId: string): Promise<ProjectRemote | null> {
  return invoke('project_remote', { projectId })
}

export function codexProjectModel(projectId: string): Promise<string | null> {
  return invoke('codex_project_model', { projectId })
}

export function scanProjects(rescan: boolean): Promise<ProjectScan> {
  return invoke<ProjectScan>('project_scan', { rescan })
}

export function localCodexProjects(): Promise<LocalCodexProject[]> {
  return invoke('local_codex_projects')
}

export function localCodexAvailable(): Promise<boolean> {
  return invoke('local_codex_available')
}

export function taskList(projectId: string): Promise<TaskList> {
  return invoke<TaskList>('task_list', { projectId })
}

export function taskWorkspace(taskId: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('task_workspace', { taskId })
}

export function saveWorkspaceLayout(taskId: string, layout: WorkspaceLayout): Promise<WorkspaceLayout> {
  return invoke('save_workspace_layout', { taskId, layout })
}

export type SplitAxis = 'horizontal' | 'vertical'
export type InspectorTab = 'terminals' | 'preview' | 'review' | 'git' | 'files'
export type TerminalSplit =
  | { type: 'pane'; terminalId: string }
  | { type: 'split'; axis: SplitAxis; ratio: number; first: TerminalSplit; second: TerminalSplit }
export interface TerminalLayout {
  revision: number
  root: TerminalSplit | null
  inspectorTab: InspectorTab
}

export function terminalLayout(projectId: string): Promise<TerminalLayout> {
  return invoke('terminal_layout', { projectId })
}

export function saveTerminalLayout(projectId: string, layout: TerminalLayout): Promise<TerminalLayout> {
  return invoke('save_terminal_layout', { projectId, layout })
}

export function saveInspectorTab(projectId: string, tab: InspectorTab): Promise<void> {
  return invoke('save_inspector_tab', { projectId, tab })
}

export function terminalList(projectId: string): Promise<TerminalList> {
  return invoke('terminal_list', { projectId })
}

export function startTerminal(
  projectId: string,
  taskId: string | null,
  columns: number,
  rows: number,
): Promise<TerminalList> {
  return invoke('start_terminal', { projectId, taskId, columns, rows })
}

export function restartTerminal(terminalId: string, columns: number, rows: number): Promise<TerminalList> {
  return invoke('restart_terminal', { terminalId, columns, rows })
}

export function startFreebuffTerminal(projectId: string): Promise<string> {
  return invoke('start_freebuff_terminal', { projectId })
}

export type SearchDirection = 'previous' | 'next'

export function searchTerminal(terminalId: string, query: string, anchor: number | null, direction: SearchDirection): Promise<number | null> {
  return invoke('search_terminal', { terminalId, query, anchor, direction })
}

export function terminalScroll(terminalId: string, lines: number): Promise<void> {
  return invoke('terminal_scroll', { terminalId, lines })
}

export function terminalFollowOutput(terminalId: string): Promise<void> {
  return invoke('terminal_follow_output', { terminalId })
}

export function terminalWrite(terminalId: string, data: string | number[], followOutput = true): Promise<void> {
  return invoke<void>('terminal_write', { terminalId, data, followOutput })
}

export function openTerminalLink(terminalId: string, row: number, column: number, revision: number): Promise<void> {
  return invoke('open_terminal_link', { terminalId, row, column, revision })
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
  const payloadLength = buffer.byteLength - 8
  if (payloadLength < 20) throw new Error('Truncated terminal frame')
  const trailer = new DataView(buffer, payloadLength)
  const revision = trailer.getUint32(0, true)
  const sequence = trailer.getUint32(4, true)
  const view = new DataView(buffer, 0, payloadLength)
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
        wraps: Boolean(attributes & 32),
        underline: Boolean(attributes & 64),
        strikeout: Boolean(attributes & 128),
      }
      offset += textLength
      cells.fill(cell, rowIndex * columns + column, rowIndex * columns + column + length)
      column += length
    }
    if (column !== columns) throw new Error('Incomplete terminal row')
  }
  const searchHighlight = (flags & 16) ? { id: integer(), start: integer(), end: integer() } : null
  if (searchHighlight && (searchHighlight.start > searchHighlight.end || searchHighlight.end >= cells.length)) {
    throw new Error('Invalid terminal search highlight')
  }
  if (offset !== payloadLength) throw new Error('Invalid terminal frame length')
  const frame: TerminalFrame = { revision, columns, rows, cursorColumn, cursorRow, cursorVisible: Boolean(flags & 1), cursorStyle: (flags >> 2) & 3, displayOffset, modes, cells, searchHighlight }
  return { frame, sequence, changedRows: full ? null : changedRows }
}

export async function subscribeTerminal(
  terminalId: string,
  handler: (frame: TerminalFrame, changedRows: number[] | null) => void,
  onError: (failure: unknown) => void,
): Promise<() => Promise<void>> {
  let closed = false
  let connection = 0
  let canReconnect = true
  let subscription: Promise<string>
  async function dispose() {
    if (closed) return
    closed = true
    await invoke<void>('unsubscribe_terminal', { subscriptionId: await subscription })
  }
  function failed(failure: unknown) {
    if (closed) return
    onError(failure)
    void dispose().catch(console.error)
  }
  function connect() {
    const generation = ++connection
    const isCurrent = () => !closed && connection === generation
    let previous: TerminalFrame | undefined
    const channel = new Channel<ArrayBuffer>((buffer) => {
      if (!isCurrent()) return
      try {
        const update = decodeTerminalFrame(buffer, previous)
        previous = update.frame
        handler(update.frame, update.changedRows)
        void Promise.resolve().then(async () => {
          const subscriptionId = await id
          if (!isCurrent()) return
          await invoke<void>('acknowledge_terminal_frame', { subscriptionId, sequence: update.sequence })
          if (isCurrent()) canReconnect = true
        }).catch((failure: unknown) => {
          if (!isCurrent()) return
          if (canReconnect && typeof failure === 'object' && failure !== null
            && 'code' in failure && failure.code === 'terminal_subscription_ended') {
            // A suspended webview may wake after the server discarded its unacknowledged frame.
            canReconnect = false
            subscription = connect()
            void subscription.catch(failed)
          } else {
            failed(failure)
          }
        })
      } catch (failure) {
        queueMicrotask(() => { if (isCurrent()) failed(failure) })
      }
    })
    const id = invoke<string>('subscribe_terminal', { terminalId, channel })
    return id
  }
  subscription = connect()
  await subscription
  return dispose
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

export interface WorkspaceFileEntry {
  name: string
  path: string
  directory: boolean
}

export interface DirectoryListing {
  entries: WorkspaceFileEntry[]
  incomplete: boolean
}

export interface TextFile {
  lines: Array<Array<{ text: string; color: string | null }>>
  truncated: boolean
}

export function listFiles(projectId: string, taskId: string | null, path: string, showDotfiles: boolean): Promise<DirectoryListing> {
  return invoke('list_files', { projectId, taskId, path, showDotfiles })
}

export function readWorkspaceFile(projectId: string, taskId: string | null, path: string): Promise<TextFile> {
  return invoke('read_workspace_file', { projectId, taskId, path })
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
