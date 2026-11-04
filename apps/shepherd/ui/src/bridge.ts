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
}

export interface ProjectList {
  projects: ProjectSnapshot[]
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

export type EntryKind =
  | { type: 'userMessage'; data: { text: string } }
  | { type: 'agentText'; data: { text: string } }
  | { type: 'thought'; data: { text: string } }
  | { type: 'toolCall'; data: ToolCallEntry }
  | { type: 'plan'; data: { rows: PlanRow[] } }
  | { type: 'permission'; data: PermissionEntry }
  | { type: 'notice'; data: { text: string; isError: boolean } }
  | { type: 'usage'; data: { used: number; size: number; cost: number | null } }

export interface Entry {
  id: string
  sequence: number
  kind: EntryKind
}

export interface TaskWorkspace {
  project: ProjectSnapshot
  task: TaskDetail
  transcript: Entry[]
}

export interface QuitSummary {
  interruptedTasks: number
  liveTerminals: number
}

export type Event =
  | { type: 'projectListChanged'; data: ProjectList }
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

export function taskList(projectId: string): Promise<TaskList> {
  return invoke<TaskList>('task_list', { projectId })
}

export function taskWorkspace(taskId: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('task_workspace', { taskId })
}

/** Subscribe the window to the core's single event stream. */
export async function subscribeToEvents(handler: (event: Event) => void): Promise<Channel<Event>> {
  const channel = new Channel<Event>()
  channel.onmessage = handler
  await invoke('subscribe', { channel })
  return channel
}
