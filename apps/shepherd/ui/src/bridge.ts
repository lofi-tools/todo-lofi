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
  foreground: number
  background: number
  bold: boolean
  italic: boolean
}

export interface TerminalFrame {
  columns: number
  rows: number
  cursorColumn: number
  cursorRow: number
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

export function terminalList(projectId: string): Promise<TerminalSnapshot[]> {
  return invoke<TerminalSnapshot[]>('terminal_list', { projectId })
}

export function startTerminal(
  projectId: string,
  columns: number,
  rows: number,
): Promise<TerminalSnapshot[]> {
  return invoke<TerminalSnapshot[]>('start_terminal', { projectId, columns, rows })
}

export function terminalWrite(terminalId: string, data: string): Promise<void> {
  return invoke<void>('terminal_write', { terminalId, data })
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

export function terminalFrame(terminalId: string): Promise<TerminalFrame | null> {
  return invoke<TerminalFrame | null>('terminal_frame', { terminalId })
}

export async function subscribeTerminal(
  terminalId: string,
  handler: (frame: TerminalFrame) => void,
): Promise<Channel<TerminalFrame>> {
  const channel = new Channel<TerminalFrame>()
  channel.onmessage = handler
  await invoke('subscribe_terminal', { terminalId, channel })
  return channel
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

export type GitMutation =
  | { type: 'stage'; path: string | null }
  | { type: 'unstage'; path: string }
  | { type: 'commit'; message: string; expected: string }
  | { type: 'checkout'; branch: string }
  | { type: 'createBranch'; branch: string }
  | { type: 'squashMerge' }

export function mutateGit(taskId: string, mutation: GitMutation): Promise<GitSnapshot> {
  return invoke('mutate_git', { taskId, mutation })
}

export type ReviewAction = { type: 'archive' } | { type: 'reopen' } | { type: 'sendNote'; text: string }

export function reviewTask(taskId: string, action: ReviewAction): Promise<TaskWorkspace> {
  return invoke('review_task', { taskId, action })
}
