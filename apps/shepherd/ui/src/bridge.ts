import { Channel, invoke as nativeInvoke, type InvokeArgs, type InvokeOptions } from '@tauri-apps/api/core'
import { reportError } from './diagnostics'

async function invoke<T>(command: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  try {
    return await nativeInvoke<T>(command, args, options)
  } catch (failure) {
    reportError(command, failure)
    throw failure
  }
}

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
export type Agent = 'codex' | 'opencode' | 'pi' | 'claude'
export const AGENT_LABELS: Record<Agent, string> = { codex: 'Codex', opencode: 'OpenCode', pi: 'Pi', claude: 'Claude Code' }

export type ShutdownState = 'running' | 'draining' | 'terminating' | 'closed'

export interface ArchivedWorktreeIssue {
  taskId: string
  title: string
  path: string
  reason: string
}

export function archivedWorktreeIssues(): Promise<ArchivedWorktreeIssue[]> {
  return invoke('archived_worktree_issues')
}

export interface PreviousProcess {
  id: string
  kind: string
  ownerId: string
  pid: number
  status: 'matching' | 'gone' | 'reused' | 'changed' | 'unknown' | 'groupUnverified'
}

export function inspectPreviousProcesses(): Promise<PreviousProcess[]> {
  return invoke('inspect_previous_processes')
}

export function stopPreviousProcess(id: string): Promise<PreviousProcess[]> {
  return invoke('stop_previous_process', { id })
}

export type Decision = 'allow' | 'allowSession' | 'allowProject' | 'deny'

export type ShortcutScope = 'window' | 'composer' | 'workspaceTerminal' | 'commitForm'
export type CommandCondition = 'idle' | 'projects' | 'project' | 'tasks' | 'task' | 'prompt' | 'cancel'
  | 'worktree' | 'terminalReady' | 'terminalPane' | 'terminalMultiple' | 'terminalFour' | 'focusedTerminal'
  | 'gitWritable' | 'diagnostics'

export interface CommandBinding {
  id: string
  label: string
  category: string
  chord: string | null
  defaultChord: string | null
  overridden: boolean
  scope: ShortcutScope
  requirements: { condition: CommandCondition; reason: string }[]
}

export interface CommandRegistry {
  commands: CommandBinding[]
  chords: Record<ShortcutScope, Record<string, string>>
  warning: string | null
}

export type KeyPlatform = 'mac' | 'linux'

export interface KeybindingEdit {
  command: string
  previous: string | null
  chord: string | null
}

export function commandRegistry(platform: KeyPlatform): Promise<CommandRegistry> {
  return invoke('command_registry', { platform })
}

export function setKeybinding(platform: KeyPlatform, edit: KeybindingEdit): Promise<CommandRegistry> {
  return invoke('set_keybinding', { platform, edit })
}

export function agentAvailability(): Promise<Record<Agent, boolean>> {
  return invoke('agent_availability')
}

export type PoolAvailabilityState = 'available' | 'exhausted' | 'cooling-down' | 'disabled' | 'unhealthy' | 'unknown'
export type PoolQuotaSource = 'catalog' | 'provider' | 'user'

export interface PoolQuotaEntry {
  id: string
  slotId: string
  observedAt: number
  source: PoolQuotaSource
  used: number | null
  limit: number | null
  limitUnit: string | null
  resetAt: number | null
  cooldownUntil: number | null
  error: string | null
}

export interface PoolSlot {
  id: string
  agent: Agent
  providerId: string
  providerName: string
  modelId: string
  modelName: string
  credentialRef: string | null
  priority: number
  enabled: boolean
  used: number | null
  limit: number | null
  limitUnit: string | null
  resetAt: number | null
  cooldownUntil: number | null
  lastUsedAt: number | null
  lastError: string | null
  createdAt: number
  updatedAt: number
}

export interface PoolSlotStatus {
  slot: PoolSlot
  availability: { state: PoolAvailabilityState; reason: string }
  latestQuota: PoolQuotaEntry | null
  currentRequest: string | null
}

export interface PoolStatus {
  generatedAt: number
  slots: PoolSlotStatus[]
  forcedSlot: string | null
}

export interface PoolReorderOutcome {
  slots: PoolSlot[]
  reason: string
}

export interface PoolSlotEnabledOutcome {
  slot: PoolSlot
  reason: string
}

export interface PoolQuotaCorrectionOutcome {
  slot: PoolSlot
  latestQuota: PoolQuotaEntry | null
  availability: { state: PoolAvailabilityState; reason: string }
  applied: boolean
  reason: string
}

export function poolStatus(): Promise<PoolStatus> {
  return invoke('pool_status')
}

export function reorderPoolSlots(slotIds: string[]): Promise<PoolReorderOutcome> {
  return invoke('pool_reorder_slots', { slotIds })
}

export function setPoolSlotEnabled(slotId: string, enabled: boolean): Promise<PoolSlotEnabledOutcome> {
  return invoke('pool_set_slot_enabled', { slotId, enabled })
}

export function correctPoolQuota(
  slotId: string,
  limit: number | null,
  limitUnit: string | null,
  resetAt: number | null,
): Promise<PoolQuotaCorrectionOutcome> {
  return invoke('pool_correct_quota', { slotId, limit, limitUnit, resetAt })
}

export function clearPoolSlotCooldown(slotId: string): Promise<PoolQuotaCorrectionOutcome> {
  return invoke('pool_clear_cooldown', { slotId })
}

export interface PoolForceOutcome {
  forcedSlot: string | null
  applied: boolean
  reason: string
}

export function forcePoolSlot(slotId: string): Promise<PoolForceOutcome> {
  return invoke('pool_force_slot', { slotId })
}

export function clearPoolForce(): Promise<PoolForceOutcome> {
  return invoke('pool_clear_force')
}

export interface ProjectSnapshot {
  id: string
  path: string
  name: string
  missing: boolean
  isGitRepository: boolean
  lastTaskId: string | null
  previewUrl: string | null
  unfinishedTaskCount: number
  reviewTaskCount: number
  runningTaskCount: number
}

export interface ProjectList {
  projects: ProjectSnapshot[]
}

export interface TaskSearchHit {
  projectId: string
  taskId: string
  entryId: string
  projectName: string
  taskTitle: string
  snippet: { before: string; matched: string; after: string; field: 'entry' | 'task' | 'project' }
}

export function reserveTaskSearch(): Promise<string> {
  return invoke<string>('reserve_task_search')
}

export function searchTasks(searchId: string, query: string): Promise<TaskSearchHit[]> {
  return nativeInvoke<TaskSearchHit[]>('search_tasks', { searchId, query })
}

export function cancelTaskSearch(searchId: string): Promise<void> {
  return invoke<void>('cancel_task_search', { searchId })
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
  attachments?: Array<{ id: string; relativePath: string; mime: string; bytes: number }>
}

export interface AgentDiagnostics {
  command: string
  protocol: string
  fallbackReason?: string
  model?: string
  reasoningEffort?: string
  stderrTail: string
}

export interface AgentCapabilities {
  streaming: boolean
  thoughts: boolean
  toolCalls: boolean
  approvals: boolean
  unreviewedTools: boolean
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

export interface AgentModes {
  current: string | null
  options: Array<{ id: string; name: string; description: string | null }>
}

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
  modes?: AgentModes
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
  | { type: 'resyncRequired' }
  | { type: 'projectListChanged'; data: ProjectList }
  | { type: 'projectApprovalsChanged'; data: { projectId: string } }
  | { type: 'terminalBell'; data: { projectId: string; taskId: string | null; terminalId: string } }
  | { type: 'terminalListChanged'; data: TerminalList }
  | { type: 'taskListChanged'; data: TaskList }
  | { type: 'taskChanged'; data: TaskSummary }
  | { type: 'taskNeedsInput'; data: { taskId: string } }
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

export function setProjectPreviewUrl(projectId: string, url: string): Promise<ProjectList> {
  return invoke<ProjectList>('set_project_preview_url', { projectId, url })
}

export function openProjectPreview(projectId: string): Promise<void> {
  return invoke<void>('open_project_preview', { projectId })
}

export function openProjectAnnotationWrapper(projectId: string, taskId: string | null): Promise<string> {
  return invoke<string>('open_project_annotation_wrapper', { projectId, taskId })
}

export interface PreviewBounds {
  x: number
  y: number
  width: number
  height: number
}

export interface PreviewAnnotationSource {
  framework?: string
  component?: string
  file?: string
  line?: number
  column?: number
}

export interface PreviewAnnotationTarget {
  selector?: string
  excerpt?: string
  bounds?: PreviewBounds
  computedStyles?: Record<string, string>
  source?: PreviewAnnotationSource
  screenshot?: PreviewBounds & { path?: string }
}

export interface PreviewAnnotationBatch {
  projectId: string
  taskId: string
  pageUrl: string
  framing?: string
  annotations: Array<{ id: string; note: string; targets: PreviewAnnotationTarget[] }>
}

export function validatePreviewAnnotations(batch: PreviewAnnotationBatch): Promise<PreviewAnnotationBatch> {
  return invoke<PreviewAnnotationBatch>('validate_preview_annotations', { batch })
}

export interface AttachmentLimit {
  bytes: number
  label: string
}

export function attachmentLimit(projectId: string): Promise<AttachmentLimit> {
  return invoke<AttachmentLimit>('attachment_limit', { projectId })
}

export async function savePreviewScreenshot(taskId: string, file: File): Promise<string> {
  return invoke<string>('save_preview_screenshot', await file.arrayBuffer(), {
    headers: { 'X-Shepherd-Task': taskId },
  })
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

export function piConfigured(): Promise<boolean> {
  return invoke<boolean>('pi_configured')
}

export function claudeConfigured(): Promise<boolean> {
  return invoke<boolean>('claude_configured')
}

export function discardDraft(taskId: string): Promise<void> {
  return invoke<void>('discard_draft', { taskId })
}

export function deleteTask(taskId: string, removeWorktree: boolean): Promise<void> {
  return invoke<void>('delete_task', { taskId, removeWorktree })
}

export interface CodexTurnOptions {
  model?: string
  effort?: string
}

export function sendPrompt(taskId: string, text: string, attachmentIds: string[] = [], codex?: CodexTurnOptions, mode?: string): Promise<TaskWorkspace> {
  return invoke<TaskWorkspace>('send_prompt', { taskId, text, attachmentIds, codex, mode })
}

export function opencodeProjectModes(projectId: string): Promise<AgentModes['options']> {
  return invoke('opencode_project_modes', { projectId })
}

export function setSessionMode(taskId: string, mode: string): Promise<TaskWorkspace> {
  return invoke('set_session_mode', { taskId, mode })
}

export async function savePromptFile(taskId: string, file: File): Promise<string> {
  return invoke<string>('save_prompt_file', await file.arrayBuffer(), {
    headers: { 'X-Shepherd-Task': taskId, 'X-Shepherd-Filename': encodeURIComponent(file.name) },
  })
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

export interface CodexModel {
  model: string
  displayName: string
  supportedReasoningEfforts: Array<{ reasoningEffort: string; description: string }>
  defaultReasoningEffort: string | null
}

export interface CodexProjectModels {
  model: string | null
  effort: string | null
  models: CodexModel[]
}

export function codexProjectModels(projectId: string): Promise<CodexProjectModels> {
  return invoke('codex_project_models', { projectId })
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
          await nativeInvoke<void>('acknowledge_terminal_frame', { subscriptionId, sequence: update.sequence })
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
  ignored: boolean
}

export interface DirectoryListing {
  entries: WorkspaceFileEntry[]
  incomplete: boolean
}

export interface FileChange {
  path: string
  kind: 'added' | 'modified' | 'deleted' | 'renamed'
  originalPath: string | null
}

export interface WorkspaceGitStatus {
  branch: string | null
  changeCount: number
}

export type FileWatchUpdate = { type: 'changes'; files: FileChange[]; git: WorkspaceGitStatus | null } | { type: 'error'; message: string }

interface WorkspaceFileWatch {
  listeners: Set<(update: FileWatchUpdate) => void>
  ready: Promise<string>
  latest?: FileWatchUpdate
}

const workspaceFileWatches = new Map<string, WorkspaceFileWatch>()

export async function watchWorkspaceFiles(projectId: string, taskId: string | null, onUpdate: (update: FileWatchUpdate, replayed: boolean) => void): Promise<() => Promise<void>> {
  const key = JSON.stringify([projectId, taskId])
  let watch = workspaceFileWatches.get(key)
  if (!watch) {
    const subscription: WorkspaceFileWatch = {
      listeners: new Set(),
      ready: Promise.resolve().then(() => {
        const channel = new Channel<FileWatchUpdate>((update) => {
          subscription.latest = update
          for (const listener of subscription.listeners) listener(update)
        })
        return invoke<string>('watch_workspace_files', { projectId, taskId, channel })
      }),
    }
    workspaceFileWatches.set(key, subscription)
    watch = subscription
  }
  const listener = (update: FileWatchUpdate) => onUpdate(update, false)
  watch.listeners.add(listener)
  if (watch.latest) onUpdate(watch.latest, true)
  try {
    const watchId = await watch.ready
    return async () => {
      if (!watch.listeners.delete(listener) || watch.listeners.size) return
      workspaceFileWatches.delete(key)
      await invoke('unwatch_workspace_files', { watchId })
    }
  } catch (error) {
    if (workspaceFileWatches.get(key) === watch) workspaceFileWatches.delete(key)
    throw error
  }
}

export interface TextFile {
  firstLine: number
  lines: Array<Array<{ text: string; color: string | null }>>
  truncated: boolean
}

export function listFiles(projectId: string, taskId: string | null, path: string, showDotfiles: boolean, speculative = false): Promise<DirectoryListing> {
  const args = { projectId, taskId, path, showDotfiles }
  return speculative ? nativeInvoke('list_files', args) : invoke('list_files', args)
}

export function readWorkspaceFile(projectId: string, taskId: string | null, path: string, line: number | null): Promise<TextFile> {
  return invoke('read_workspace_file', { projectId, taskId, path, line })
}

export type FileAction = 'edit' | 'reveal'

export function actOnWorkspaceFile(projectId: string, taskId: string | null, path: string, action: FileAction): Promise<void> {
  return invoke('act_on_workspace_file', { projectId, taskId, path, action })
}

export interface FileSearchQuery {
  mode: 'name' | 'contents' | 'path'
  query: string
  showDotfiles: boolean
}

export interface FileSearchResult {
  matches: Array<{ path: string; line: number | null; excerpt: string | null }>
  incomplete: boolean
}

export function reserveFileSearch(): Promise<string> {
  return invoke('reserve_file_search')
}

export function searchWorkspaceFiles(searchId: string, projectId: string, taskId: string | null, query: FileSearchQuery): Promise<FileSearchResult> {
  return nativeInvoke('search_workspace_files', { searchId, projectId, taskId, query })
}

export function cancelFileSearch(searchId: string): Promise<void> {
  return invoke('cancel_file_search', { searchId })
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

export function reviewTaskLists(): Promise<TaskList[]> {
  return invoke('review_task_lists')
}

export function archiveReviewed(taskIds: string[]): Promise<void> {
  return invoke('archive_reviewed', { taskIds })
}

export interface SettingsValues {
  theme: 'system' | 'light' | 'dark'
  'appearance.density': 'compact' | 'comfortable'
  'tasks.show_archived': boolean
  'appearance.send_accent': 'Blue' | 'Violet' | 'Teal' | 'Amber' | 'Neutral'
  'notifications.enabled': boolean
  'attachments.max_file_bytes': number
  'terminal.scrollback_lines': number
  'files.show_dotfiles': boolean
}

export type SettingKey = keyof SettingsValues

export interface SettingsSnapshot {
  app: Partial<SettingsValues>
  projects: Record<string, Partial<SettingsValues>>
  defaults: SettingsValues
  warning: string | null
}

export interface SettingsEdit {
  projectId: string | null
  key: SettingKey
  previous: SettingsValues[SettingKey] | null
  value: SettingsValues[SettingKey] | null
}

export function reloadSettings(): Promise<SettingsSnapshot> {
  return invoke('reload_settings')
}

export function setSetting(edit: SettingsEdit): Promise<SettingsSnapshot> {
  return invoke('set_setting', { edit })
}

export function setTerminalAppearance(light: boolean, moreContrast: boolean): Promise<void> {
  return invoke('set_terminal_appearance', { appearance: { light, moreContrast } })
}

export function notifyTaskNeedsInput(taskId: string): Promise<void> {
  return invoke('notify_task_needs_input', { taskId })
}
