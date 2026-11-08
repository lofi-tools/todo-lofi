/**
 * The fixed task-workspace shell. Regions never move between each other; every
 * value comes from the core's snapshots through the bridge, never from a second
 * frontend model.
 */

import { For, Show, batch, createEffect, createMemo, createResource, createSignal, createUniqueId, onCleanup, onMount, type JSX } from 'solid-js'
import { isTauri } from '@tauri-apps/api/core'
import { homeDir } from '@tauri-apps/api/path'
import { getCurrentWindow } from '@tauri-apps/api/window'

import * as bridge from './bridge'
import { CommandPalette, type PaletteEntry } from './CommandPalette'
import { displayChord, normalizeChord } from './keybindings'
import { ComposerInput } from './ComposerInput'
import { FilesPane } from './FilesPane'
import { ProjectImportSources, ProjectPicker } from './ProjectPicker'
import { ProjectRow } from './ProjectRow'
import { PreviewPane, type PreviewAnnotation } from './PreviewPane'
import { GitPane } from './GitPane'
import { ReviewPane } from './ReviewPane'
import { SEND_ACCENTS, SettingsPane } from './SettingsPane'
import { FreebuffPane, TerminalPane, type TerminalCommands } from './TerminalPane'
import { TaskSearch } from './TaskSearch'
import { DeleteTaskDialog, ReviewArchiveDialog, TaskRows } from './TaskRows'
import { ErrorNotifications } from './ErrorNotifications'
import { reportError } from './diagnostics'

type CoreEvent = bridge.Event
type TerminalBell = Extract<CoreEvent, { type: 'terminalBell' }>['data']
type TurnSelection = { codex?: bridge.CodexTurnOptions; mode?: string }
type PaletteKind = 'commands' | 'projects' | 'tasks'

const LIVE_STATES: bridge.TaskState[] = ['starting', 'queued', 'working', 'waiting']
const TIER_LABELS: Record<bridge.AgentTier, string> = {
  native: 'Native',
  acp: 'ACP',
  'terminal-wrapper': 'Terminal wrapper',
  'plain-pane': 'Plain terminal',
}
const RECOVERY_LABELS: Record<bridge.SessionRecovery, string> = {
  resume: 'Resume',
  reconnect: 'Reconnect',
  startNew: 'Start new session',
}
const CAPABILITY_LABELS: Record<keyof bridge.AgentCapabilities, string> = {
  streaming: 'Streaming deltas',
  thoughts: 'Thoughts',
  toolCalls: 'Tool activity',
  approvals: 'Tool approvals',
  resume: 'Resume',
  cancel: 'Cancel turn',
  modelPicker: 'Model selection',
  modePicker: 'Mode selection',
  images: 'Images',
  resourceLinks: 'Resource links',
  usage: 'Usage',
  agentTerminals: 'Agent terminals',
  patchesInApprovals: 'Approval patches',
}
const SEND_ACCENT_KEY = 'shepherd.sendAccent'
const APPROVAL_LABELS: Record<bridge.Decision, string> = {
  allow: 'Allow once',
  allowSession: 'Allow for session',
  allowProject: 'Always allow for project',
  deny: 'Deny',
}
const DEFAULT_APPROVAL_CHOICES: bridge.Decision[] = ['allow', 'deny']
const AGENT_OPTIONS: PickerOption[] = [
  { id: 'codex', name: bridge.AGENT_LABELS.codex },
  { id: 'opencode', name: bridge.AGENT_LABELS.opencode },
  { id: 'pi', name: bridge.AGENT_LABELS.pi },
  { id: 'claude', name: bridge.AGENT_LABELS.claude },
  { id: 'freebuff', name: 'Freebuff (terminal)' },
]

function appearanceStorage() {
  return import.meta.env.VITE_SHEPHERD_EPHEMERAL === '1' ? sessionStorage : localStorage
}

function terminalArrowDestination(inspector: HTMLElement, tab: HTMLButtonElement, target: HTMLElement, key: string) {
  const selector = 'button[data-terminal-focus]:enabled'
  if (target === tab) {
    return key === 'ArrowDown' ? inspector.querySelector<HTMLButtonElement>(selector) : null
  }
  const header = target.closest('.terminal-pane__bar')
  const current = header?.closest('.terminal-tile')
  if (!current || !header?.querySelector(selector)) return null

  const horizontal = key === 'ArrowLeft' || key === 'ArrowRight'
  const forward = key === 'ArrowRight' || key === 'ArrowDown'
  const origin = current.getBoundingClientRect()
  const originStart = horizontal ? origin.top : origin.left
  const originEnd = horizontal ? origin.bottom : origin.right
  const crossCenter = (originStart + originEnd) / 2
  let closest: HTMLButtonElement | null = null
  let shortest = Infinity
  for (const button of inspector.querySelectorAll<HTMLButtonElement>(selector)) {
    const pane = button.closest('.terminal-tile')
    if (!pane || pane === current) continue
    const bounds = pane.getBoundingClientRect()
    const gap = horizontal
      ? (forward ? bounds.left - origin.right : origin.left - bounds.right)
      : (forward ? bounds.top - origin.bottom : origin.top - bounds.bottom)
    if (gap < 0) continue
    const crossStart = horizontal ? bounds.top : bounds.left
    const crossEnd = horizontal ? bounds.bottom : bounds.right
    if (crossEnd <= originStart || crossStart >= originEnd) continue
    const distance = Math.hypot(gap, Math.max(0, crossStart - crossCenter, crossCenter - crossEnd))
    if (distance < shortest) {
      closest = button
      shortest = distance
    }
  }
  return closest ?? (key === 'ArrowUp' ? tab : null)
}

export function App() {
  const [projects, setProjects] = createSignal<bridge.ProjectSnapshot[]>([])
  const [previousProcesses, setPreviousProcesses] = createSignal<bridge.PreviousProcess[]>([])
  const [worktreeIssues, setWorktreeIssues] = createSignal<bridge.ArchivedWorktreeIssue[]>([])
  const [worktreeNoticeDismissed, setWorktreeNoticeDismissed] = createSignal(false)
  const [checkingProcesses, setCheckingProcesses] = createSignal(false)
  const [processNoticeDismissed, setProcessNoticeDismissed] = createSignal(false)
  const processesNeedingReview = () => previousProcesses().filter((process) =>
    process.status === 'matching' || process.status === 'unknown' || process.status === 'changed' || process.status === 'groupUnverified')
  async function inspectPreviousProcesses() {
    setCheckingProcesses(true)
    try {
      setPreviousProcesses(await bridge.inspectPreviousProcesses())
    } catch (failure) {
      setError(describe(failure))
    } finally {
      setCheckingProcesses(false)
    }
  }
  const [stoppingProcess, setStoppingProcess] = createSignal<string>()
  async function stopPreviousProcess(id: string) {
    setStoppingProcess(id)
    try {
      setPreviousProcesses(await bridge.stopPreviousProcess(id))
    } catch (failure) {
      setError(describe(failure))
    } finally {
      setStoppingProcess()
    }
  }
  const [homeDirectory, setHomeDirectory] = createSignal<string>()
  const [projectsLoaded, setProjectsLoaded] = createSignal(false)
  const [codexAvailable, setCodexAvailable] = createSignal(false)
  const [opencodeConfigured, setOpencodeConfigured] = createSignal(false)
  const [piConfigured, setPiConfigured] = createSignal(false)
  const [claudeConfigured, setClaudeConfigured] = createSignal(false)
  const [newTaskAgent, setNewTaskAgent] = createSignal<bridge.Agent>('codex')
  const [selectedProjectId, setSelectedProjectId] = createSignal<string | null>(null)
  const [freebuffProjectId, setFreebuffProjectId] = createSignal<string>()
  const selectedProject = () => projects().find((project) => project.id === selectedProjectId())
  const [projectModel] = createResource(
    () => {
      const project = selectedProject()
      return project && !project.missing ? project.id : false
    },
    async (projectId) => {
      try {
        return { projectId, ...await bridge.codexProjectModels(projectId) }
      } catch (error) {
        console.warn('Could not read the Codex model', error)
        return { projectId, model: null, effort: null, models: [] }
      }
    },
  )
  const [tasks, setTasks] = createSignal<bridge.TaskSummary[]>([])
  const [reviewOnly, setReviewOnly] = createSignal(false)
  const [reviewLists, setReviewLists] = createSignal(new Map<string, bridge.TaskSummary[]>())
  const [loadingReview, setLoadingReview] = createSignal(false)
  const [reviewSelection, setReviewSelection] = createSignal(new Set<string>())
  const [archiveConfirmation, setArchiveConfirmation] = createSignal<string[]>()
  let reviewLoad: Map<string, bridge.TaskSummary[]> | undefined
  let reviewAnchor: string | undefined
  let archiveSelectedButton: HTMLButtonElement | undefined
  let reviewCountButton: HTMLButtonElement | undefined
  const [loadedTasksFor, setLoadedTasksFor] = createSignal<string>()
  const [selectedTaskId, setSelectedTaskId] = createSignal<string | null>(null)
  const [workspace, setWorkspace] = createSignal<bridge.TaskWorkspace | null>(null)
  const [deleteRequest, setDeleteRequest] = createSignal<bridge.TaskDetail>()
  const [searchOpen, setSearchOpen] = createSignal(false)
  const [palette, setPalette] = createSignal<PaletteKind>()
  const [commandRegistry, setCommandRegistry] = createSignal<bridge.CommandRegistry>()
  const [terminalCommands, setTerminalCommands] = createSignal<TerminalCommands>()
  const [sidebarHidden, setSidebarHidden] = createSignal(false)
  const [commandNotice, setCommandNotice] = createSignal('')
  const [gitRequest, setGitRequest] = createSignal<{ taskId: string; action: 'stageAll' | 'commit' }>()
  let paletteOpener: HTMLElement | undefined
  let forwardNextKeyTo: HTMLElement | undefined
  let forwardedKey: string | undefined
  let sidebar: HTMLElement | undefined
  let diagnostics: HTMLDetailsElement | undefined
  const [searchJump, setSearchJump] = createSignal<bridge.TaskSearchHit>()
  let searchInput: HTMLInputElement | undefined
  let searchOpener: HTMLElement | undefined
  let transcript: HTMLDivElement | undefined
  const currentAgent = () => workspace()?.task.agent ?? newTaskAgent()
  const currentAgentName = () => bridge.AGENT_LABELS[currentAgent()]
  const codexCatalog = () => {
    const configured = projectModel()
    return configured?.projectId === selectedProjectId() ? configured : undefined
  }
  const [projectMenuId, setProjectMenuId] = createSignal<string>()
  const [settingsOpen, setSettingsOpen] = createSignal(false)
  const [preferences, setPreferences] = createSignal<bridge.SettingsSnapshot>()
  const prefersLight = window.matchMedia('(prefers-color-scheme: light)')
  const prefersContrast = window.matchMedia('(prefers-contrast: more)')
  const [systemLight, setSystemLight] = createSignal(prefersLight.matches)
  const [moreContrast, setMoreContrast] = createSignal(prefersContrast.matches)
  const lightTheme = createMemo(() => {
    const selected = preferences()?.app.theme ?? 'system'
    return selected === 'light' || (selected === 'system' && systemLight())
  })
  const density = createMemo(() => preferences()?.app['appearance.density'] ?? 'compact')
  let pendingTerminalAppearance = Promise.resolve()
  onMount(() => {
    const light = (event: MediaQueryListEvent) => setSystemLight(event.matches)
    const contrast = (event: MediaQueryListEvent) => setMoreContrast(event.matches)
    prefersLight.addEventListener('change', light)
    prefersContrast.addEventListener('change', contrast)
    onCleanup(() => {
      prefersLight.removeEventListener('change', light)
      prefersContrast.removeEventListener('change', contrast)
    })
  })
  createEffect(() => {
    const light = lightTheme()
    const contrast = moreContrast()
    document.documentElement.dataset.theme = light ? 'light' : 'dark'
    document.documentElement.dataset.contrast = contrast ? 'more' : 'normal'
    pendingTerminalAppearance = pendingTerminalAppearance
      .then(() => bridge.setTerminalAppearance(light, contrast))
      .catch((failure) => { setError(describe(failure)) })
  })
  createEffect(() => { document.documentElement.dataset.density = density() })
  const [savingPreference, setSavingPreference] = createSignal(false)
  const [settingsConflicts, setSettingsConflicts] = createSignal<Omit<bridge.SettingsEdit, 'previous'>[]>([])
  const preferencesEditable = () => !!preferences() && !preferences()?.warning && !savingPreference()
  const notificationsEnabled = () => preferences()?.app['notifications.enabled'] ?? false
  const sendAccent = () => preferences()?.app['appearance.send_accent'] ?? 'Blue'
  const [projectSettingsRequest, setProjectSettingsRequest] = createSignal<{ id: string }>()
  const [approvalChange, setApprovalChange] = createSignal<{ projectId: string }>()
  const showsArchived = (projectId: string | null) => {
    const path = projects().find((project) => project.id === projectId)?.path
    return (path ? preferences()?.projects[path]?.['tasks.show_archived'] : undefined)
      ?? preferences()?.app['tasks.show_archived'] ?? false
  }
  const settingsProject = () => projects().find((project) => project.id === (projectSettingsRequest()?.id ?? selectedProjectId()))
  const visibleTasks = () => tasks().filter((task) => showsArchived(selectedProjectId()) || task.state !== 'done')

  function openProjectSettings(projectId: string) {
    setProjectSettingsRequest({ id: projectId })
    setSettingsOpen(true)
  }
  const [terminalListEvent, setTerminalListEvent] = createSignal<Extract<CoreEvent, { type: 'terminalListChanged' }>['data']>()
  const [inspectorTabs, setInspectorTabs] = createSignal(new Map<string | null, bridge.InspectorTab>())
  const [fileRequest, setFileRequest] = createSignal<{ taskId: string; path: string }>()
  const inspectorTab = () => inspectorTabs().get(selectedProjectId()) ?? 'terminals'
  let pendingInspectorTabSave = Promise.resolve()
  let inspector: HTMLElement | undefined
  let inspectorTabList: HTMLDivElement | undefined
  const [emptyInspectorWidth, setEmptyInspectorWidth] = createSignal<number | null>(null)
  const inspectorWidth = () => selectedTaskId()
    ? workspace()?.layout.inspectorWidth ?? null
    : emptyInspectorWidth()

  function setInspectorTab(tab: bridge.InspectorTab) {
    const projectId = selectedProjectId()
    setInspectorTabs((tabs) => new Map(tabs).set(projectId, tab))
    if (!projectId) return
    pendingInspectorTabSave = pendingInspectorTabSave
      .then(() => bridge.saveInspectorTab(projectId, tab))
      .catch((failure) => {
        setError(`Could not save the inspector tab. ${describe(failure)}`)
      })
  }

  function restoreInspectorTab(projectId: string, tab: bridge.InspectorTab) {
    if (!INSPECTOR_TABS.some((option) => option.id === tab && option.enabled)) return
    setInspectorTabs((tabs) => tabs.has(projectId) ? tabs : new Map(tabs).set(projectId, tab))
  }

  function openTaskFile(path: string) {
    const task = workspace()?.task
    if (!task?.worktreePath || task.id !== selectedTaskId()) return
    setFileRequest({ taskId: task.id, path })
    setInspectorTab('files')
    inspectorTabList?.querySelector<HTMLButtonElement>('#inspector-tab-files')?.focus()
  }

  async function resizeInspector(width: number) {
    const taskId = selectedTaskId()
    if (!taskId) {
      setEmptyInspectorWidth(width)
      return
    }
    setError(null)
    try {
      const layout = await bridge.saveWorkspaceLayout(taskId, { inspectorWidth: width })
      setWorkspace((current) => current?.task.id === taskId ? { ...current, layout } : current)
    } catch (failure) {
      setError(describe(failure))
    }
  }
  const [terminalAttention, setTerminalAttention] = createSignal<TerminalBell[]>([])
  const [focusedTerminal, setFocusedTerminal] = createSignal<string | null>(null)
  const projectAttention = () => terminalAttention().filter((bell) => bell.projectId === selectedProjectId())
  const tabAttention = () => projectAttention().some((bell) => bell.taskId === null || bell.taskId === selectedTaskId())

  function focusTerminal(id: string | null) {
    setFocusedTerminal(id)
    if (id) setTerminalAttention((bells) => bells.filter((bell) => bell.terminalId !== id))
  }
  const [prompt, setPrompt] = createSignal('')
  const [queuedFiles, setQueuedFiles] = createSignal(new Map<string, File[]>())
  const fileQueueKey = () => {
    const task = selectedTaskId()
    const project = selectedProjectId()
    return task ? `task:${task}` : project ? `project:${project}` : undefined
  }
  const promptFiles = () => queuedFiles().get(fileQueueKey() ?? '') ?? []
  const [queuedTurnOptions, setQueuedTurnOptions] = createSignal(new Map<string, TurnSelection>())
  const turnOptions = () => queuedTurnOptions().get(fileQueueKey() ?? '')
  const codexOptions = () => turnOptions()?.codex
  const pendingMode = () => currentAgent() === 'opencode' ? turnOptions()?.mode : undefined
  const [projectModes] = createResource(
    () => {
      const project = selectedProject()
      const task = workspace()?.task
      return opencodeConfigured() && currentAgent() === 'opencode' && project && !project.missing
        && (!task || task.state === 'draft') ? project.id : false
    },
    async (projectId) => {
      try {
        return { projectId, options: await bridge.opencodeProjectModes(projectId) }
      } catch (error) {
        console.warn('Could not read OpenCode project modes', error)
        return { projectId, options: [] }
      }
    },
  )
  const composerModes = (): bridge.AgentModes | undefined => {
    const current = workspace()
    const selected = pendingMode()
    if (current?.modes) return selected ? { ...current.modes, current: selected } : current.modes
    if (currentAgent() !== 'opencode' || (current && current.task.state !== 'draft' && !selected)) return
    const catalog = projectModes()
    const options = catalog && catalog.projectId === selectedProjectId() ? catalog.options : []
    if (options.length || selected) return { current: selected ?? null, options }
  }
  const composerModel = () => codexOptions()?.model ?? workspace()?.diagnostics?.model ?? codexCatalog()?.model ?? null
  const composerEffort = () => {
    const queued = codexOptions()
    if (queued?.effort || queued?.model) return queued.effort
    const catalog = codexCatalog()
    return workspace()?.diagnostics?.reasoningEffort
      ?? (catalog?.model === composerModel() ? catalog?.effort : undefined)
  }

  function setTurnOptions(options: TurnSelection) {
    const key = fileQueueKey()
    if (key) setQueuedTurnOptions((queued) => new Map(queued).set(key, options))
  }

  function clearTurnOptions(taskId: string) {
    setQueuedTurnOptions((queued) => {
      const key = `task:${taskId}`
      if (!queued.has(key)) return queued
      const next = new Map(queued)
      next.delete(key)
      return next
    })
  }

  function moveTurnOptionsToDraft(projectId: string, taskId: string) {
    setQueuedTurnOptions((queued) => {
      const key = `project:${projectId}`
      const options = queued.get(key)
      if (!options) return queued
      const next = new Map(queued)
      next.delete(key)
      return next.set(`task:${taskId}`, options)
    })
  }
  let fileInput: HTMLInputElement | undefined
  let promptInput: HTMLTextAreaElement | undefined
  const firstDrafts = new Map<string, Promise<bridge.TaskWorkspace>>()
  const [pickerOpen, setPickerOpen] = createSignal(false)
  const [scan, setScan] = createSignal<bridge.ProjectScan | null>(null)
  const [scanning, setScanning] = createSignal(false)
  const [scanError, setScanError] = createSignal<string | null>(null)
  const [importNote, setImportNote] = createSignal<string | null>(null)
  function setError(message: string | null) {
    if (message) reportError('workspace', message)
  }
  const attachmentLimitKey = createMemo(
    () => {
      const project = selectedProject()
      if (!project) return undefined
      const snapshot = preferences()
      return [project.id, snapshot?.projects[project.path]?.['attachments.max_file_bytes'] ?? snapshot?.app['attachments.max_file_bytes']] as const
    },
    undefined,
    { equals: (previous, next) => previous?.[0] === next?.[0] && previous?.[1] === next?.[1] },
  )
  const [projectAttachmentLimit] = createResource(
    attachmentLimitKey,
    async ([projectId]) => {
      try {
        return { projectId, limit: await bridge.attachmentLimit(projectId) }
      } catch (failure) {
        return { projectId, error: describe(failure) }
      }
    },
  )
  const attachmentLimit = () => {
    const settings = projectAttachmentLimit()
    return settings && settings.projectId === selectedProjectId() ? settings.limit : undefined
  }
  createEffect(() => {
    const settings = projectAttachmentLimit()
    if (settings?.projectId === selectedProjectId() && settings?.error) setError(settings.error)
  })
  const [pending, setPending] = createSignal(false)
  const [cancelRequestTaskId, setCancelRequestTaskId] = createSignal<string>()
  const [quitRequired, setQuitRequired] = createSignal<bridge.QuitConfirmation | null>(null)
  const workspaceShortcutsEnabled = () =>
    !settingsOpen() && !freebuffProjectId() && !pickerOpen() && !searchOpen() && !quitRequired() && !palette()
  const [shutdownNote, setShutdownNote] = createSignal<string | null>(null)
  let allowClose = false

  const agentConfigured = (agent: bridge.Agent) => {
    if (agent === 'codex') return true
    if (agent === 'opencode') return opencodeConfigured()
    if (agent === 'pi') return piConfigured()
    return claudeConfigured()
  }

  const agentOptions = () => {
    const project = selectedProject()
    const task = workspace()?.task
    const canOpen = project && !project.missing && !pending()
    const canChange = canOpen && (!task || task.state === 'draft')
    return AGENT_OPTIONS.map((agent) => {
      if (agent.id === 'freebuff') return { ...agent, disabled: !canOpen }
      if (agent.id === 'codex') return { ...agent, disabled: !canChange }
      if (agent.id === 'opencode' || agent.id === 'pi' || agent.id === 'claude') return {
        ...agent,
        disabled: !canChange || !agentConfigured(agent.id),
        detail: agentConfigured(agent.id) ? undefined : 'Set the executable in settings.json',
      }
      return agent
    })
  }

  function chooseAgent(agent: string) {
    const project = selectedProject()
    if (!project || project.missing || pending()) return
    if (agent === 'freebuff') {
      setFreebuffProjectId(project.id)
      return
    }
    if (agent !== 'codex' && agent !== 'opencode' && agent !== 'pi' && agent !== 'claude') return
    if (!agentConfigured(agent)) return
    const task = workspace()?.task
    if (!task) {
      setNewTaskAgent(agent)
    } else if (task.state === 'draft' && task.agent !== agent) {
      void run(async () => {
        const updated = await bridge.setDraftAgent(task.id, agent)
        if (selectedTaskId() === task.id) setWorkspace(updated)
      })
    }
  }

  async function reloadPreferences() {
    if (savingPreference()) return
    setError(null)
    setSavingPreference(true)
    try {
      let snapshot = await bridge.reloadSettings()
      if (!snapshot.warning) {
        try {
          const stored = appearanceStorage().getItem(SEND_ACCENT_KEY)
          const legacy = SEND_ACCENTS.find((accent) => accent === stored)
          if (legacy && snapshot.app['appearance.send_accent'] === undefined) {
            snapshot = await bridge.setSetting({ projectId: null, key: 'appearance.send_accent', previous: null, value: legacy })
          }
          if (legacy) appearanceStorage().removeItem(SEND_ACCENT_KEY)
        } catch (failure) {
          setError(`Could not migrate the saved Send accent. ${describe(failure)}`)
        }
      }
      setPreferences(snapshot)
    } finally {
      setSavingPreference(false)
    }
  }

  async function editPreference(key: bridge.SettingKey, value: bridge.SettingsEdit['value'], projectId: string | null = null) {
    if (!preferencesEditable()) throw new Error('Settings are unavailable for editing.')
    const path = projectId ? projects().find((project) => project.id === projectId)?.path : undefined
    const scope = path ? preferences()?.projects[path] : preferences()?.app
    const edit = { key, projectId, value, previous: scope?.[key] ?? null }
    setSavingPreference(true)
    setError(null)
    try {
      setPreferences(await bridge.setSetting(edit))
      discardSettingsConflict(edit)
    } catch (failure) {
      if ((failure as bridge.BridgeError | null)?.code === 'settings_conflict') {
        discardSettingsConflict(edit)
        setSettingsConflicts((current) => [...current, { key, projectId, value }])
      }
      throw failure
    } finally {
      setSavingPreference(false)
    }
  }

  function discardSettingsConflict(edit: Pick<bridge.SettingsEdit, 'key' | 'projectId'>) {
    setSettingsConflicts((current) => current.filter(({ key, projectId }) => key !== edit.key || projectId !== edit.projectId))
  }

  function requestPreference(key: bridge.SettingKey, value: bridge.SettingsEdit['value'], projectId: string | null = null) {
    return editPreference(key, value, projectId).catch((failure) => { setError(describe(failure)) })
  }

  const runningCount = createMemo(() => projects().reduce((count, project) => count + (project.runningTaskCount ?? 0), 0))
  const windowTitle = createMemo(() => {
    const project = selectedProject()?.name
    const title = project ? `${project} — Shepherd` : 'Shepherd'
    return runningCount() ? `${title} (${runningCount()} working)` : title
  })
  let pendingWindowTitle = Promise.resolve()
  createEffect(() => {
    const title = windowTitle()
    document.title = title
    if (isTauri()) pendingWindowTitle = pendingWindowTitle.then(() => getCurrentWindow().setTitle(title))
      .catch((failure) => console.warn('Could not set the Shepherd window title', failure))
  })

  const worktreePath = createMemo(() => {
    const task = workspace()?.task
    return task && task.id === selectedTaskId() && task.projectId === selectedProjectId() && !selectedProject()?.missing
      ? task.worktreePath : null
  })
  const [worktreeGit, setWorktreeGit] = createSignal<bridge.WorkspaceGitStatus>()
  const [gitStatusError, setGitStatusError] = createSignal<string>()
  createEffect(() => {
    const path = worktreePath()
    const taskId = selectedTaskId()
    const projectId = selectedProjectId()
    setWorktreeGit(undefined)
    setGitStatusError(undefined)
    if (!path || !taskId || !projectId) return
    let closed = false
    const watch = bridge.watchWorkspaceFiles(projectId, taskId, (update) => {
      if (closed) return
      setWorktreeGit(update.type === 'changes' ? update.git ?? undefined : undefined)
      setGitStatusError(update.type === 'error' ? update.message : update.git ? undefined : 'The task worktree has no Git metadata.')
    })
    void watch.catch((failure) => { if (!closed) setGitStatusError(describe(failure)) })
    onCleanup(() => {
      closed = true
      void watch.then((dispose) => dispose(), () => undefined)
        .catch((failure) => console.warn('Could not stop the worktree status watcher', failure))
    })
  })
  const reviewCount = createMemo(() => projects().reduce((count, project) => count + (project.reviewTaskCount ?? 0), 0))
  const filteredReviewTasks = (projectId: string) => reviewLists().get(projectId) ?? []
  const reviewRows = () => projects().flatMap((project) => filteredReviewTasks(project.id))
  const selectedReviewIds = () => reviewRows().filter((task) => reviewSelection().has(task.id)).map((task) => task.id)
  const sidebarSelection = () => reviewOnly() ? reviewSelection() : undefined
  const sidebarProjects = () => reviewOnly()
    ? projects().filter((project) => filteredReviewTasks(project.id).length > 0)
    : projects()
  const canStartTask = () => {
    const project = selectedProject()
    return !!project && !project.missing && loadedTasksFor() === project.id
      && !selectedTaskId() && visibleTasks().length === 0
  }
  const canCompose = () => {
    if (selectedProject()?.missing) return false
    const state = workspace()?.task.state
    return state === 'draft' || state === 'ready' || canStartTask()
      || (!selectedTaskId() && firstDrafts.has(selectedProjectId() ?? ''))
  }
  const emptyChatMessage = () => {
    if (!projectsLoaded()) return pending() ? 'Loading projects…' : 'Projects are unavailable.'
    const project = selectedProject()
    if (!project) return 'Add a project to start.'
    if (project.missing) return 'The project folder is unavailable.'
    if (selectedTaskId()) return pending() ? 'Opening task…' : 'This task could not be opened.'
    if (loadedTasksFor() !== project.id) return pending() ? 'Loading project tasks…' : 'Project tasks are unavailable.'
    return 'Choose a task from Projects or use Compose to start a new one.'
  }
  const canAddPreviewNote = () => canCompose() && !pending() && !settingsOpen() && !freebuffProjectId()
  const canCancel = () => {
    const current = workspace()
    return !!current?.capabilities?.cancel && LIVE_STATES.includes(current.task.state)
  }
  const canApprove = () => {
    const current = workspace()
    return !!current?.capabilities?.approvals && current.task.state === 'waiting'
  }
  const showShellApprovals = () => {
    const current = workspace()
    return current ? current.task.state === 'draft' || current.capabilities?.approvals === true : !selectedTaskId()
  }
  const cancellingTurn = () => {
    const current = workspace()
    return !!current && (current.cancelling || cancelRequestTaskId() === current.task.id)
  }
  const mac = navigator.platform.startsWith('Mac')
  const keyPlatform = mac ? 'mac' : 'linux'

  async function reloadKeybindings() {
    const registry = await bridge.commandRegistry(keyPlatform)
    setCommandRegistry(registry)
    return registry
  }

  async function editKeybinding(edit: bridge.KeybindingEdit) {
    const registry = await bridge.setKeybinding(keyPlatform, edit)
    setCommandRegistry(registry)
  }
  const bindingsById = createMemo(() => new Map(commandRegistry()?.commands.map((binding) => [binding.id, binding])))
  const chordFor = (id: string) => bindingsById().get(id)?.chord ?? undefined
  const canSend = () => canCompose() && !pending() && (!!prompt().trim() || !!promptFiles().length)
  const canUseGit = () => {
    const task = workspace()?.task
    return !!task?.worktreePath && task.state !== 'done' && !LIVE_STATES.includes(task.state)
  }

  function workspaceTerminal(target: unknown): target is HTMLTextAreaElement {
    return target instanceof HTMLTextAreaElement && target.matches('.inspector .terminal-view__input')
  }

  const commandConditions = (focus: unknown = paletteOpener): Record<bridge.CommandCondition, boolean> => {
    const project = selectedProject()
    const current = workspace()
    const terminal = terminalCommands()?.available()
    return {
      idle: !pending(),
      projects: projects().length > 0 && !pending(),
      project: !!project && !project.missing,
      tasks: visibleTasks().length > 0 && !pending(),
      task: !!current,
      prompt: !settingsOpen() && canSend(),
      cancel: !settingsOpen() && canCancel() && !cancellingTurn(),
      worktree: !!current?.task.worktreePath,
      terminalReady: !!terminal?.terminalReady,
      terminalPane: !!terminal?.terminalPane,
      terminalMultiple: !!terminal?.terminalMultiple,
      terminalFour: !!terminal?.terminalFour,
      focusedTerminal: workspaceTerminal(focus) && focus.isConnected,
      gitWritable: canUseGit(),
      diagnostics: !!current?.diagnostics,
    }
  }

  function disabledReason(binding: bridge.CommandBinding, conditions: ReturnType<typeof commandConditions>) {
    return binding.requirements.find(({ condition }) => !conditions[condition])?.reason
  }

  const paletteEntries = createMemo<PaletteEntry[]>(() => {
    switch (palette()) {
      case 'projects': return projects().map((project) => ({
        id: project.id, label: project.name, detail: project.path,
        disabledReason: project.missing ? 'Project path is missing.' : undefined,
      }))
      case 'tasks': return visibleTasks().map((task) => ({
        id: task.id, label: task.title, detail: task.state,
      }))
      case 'commands': {
        const conditions = commandConditions()
        return (commandRegistry()?.commands ?? []).map((binding) => ({
          id: binding.id, label: binding.label, category: binding.category,
          binding: binding.chord ? displayChord(binding.chord, mac) : undefined,
          disabledReason: disabledReason(binding, conditions),
        }))
      }
      default: return []
    }
  })

  function openPalette(kind: PaletteKind) {
    if (freebuffProjectId() || pickerOpen() || searchOpen() || quitRequired() || archiveConfirmation()) return
    if (!palette()) paletteOpener = document.activeElement instanceof HTMLElement ? document.activeElement : undefined
    setPalette(kind)
  }

  function closePalette() {
    setPalette(undefined)
    paletteOpener?.focus({ preventScroll: true })
    paletteOpener = undefined
  }

  function choosePaletteEntry(entry: PaletteEntry) {
    switch (palette()) {
      case 'projects': {
        closePalette()
        const project = projects().find(({ id }) => id === entry.id)
        if (project) openProject(project, false)
        break
      }
      case 'tasks': {
        closePalette()
        const task = visibleTasks().find(({ id }) => id === entry.id)
        if (task) openTaskRow(task)
        break
      }
      case 'commands': {
        const binding = bindingsById().get(entry.id)
        if (!binding || disabledReason(binding, commandConditions())) return
        if (entry.id === 'switchProject' || entry.id === 'switchTask') {
          openPalette(entry.id === 'switchProject' ? 'projects' : 'tasks')
          return
        }
        const opener = paletteOpener
        closePalette()
        if (entry.id !== 'commandPalette') runWorkspaceCommand(entry.id, opener, false)
        break
      }
    }
  }

  function toggleSidebar() {
    const sidebarFocused = sidebar?.contains(document.activeElement)
    setSidebarHidden((hidden) => !hidden)
    if (sidebarHidden() && sidebarFocused) {
      const next = settingsOpen()
        ? document.querySelector<HTMLElement>('.settings__nav button[aria-current="true"]')
        : promptInput?.disabled ? transcript : promptInput
      next?.focus({ preventScroll: true })
    }
  }

  function focusPane(target: unknown, step: number) {
    const tab = inspectorTabList?.querySelector<HTMLButtonElement>('button[aria-selected="true"]')
    const terminalStops = inspectorTab() === 'terminals' && inspector
      ? Array.from(inspector.querySelectorAll<HTMLTextAreaElement>('.terminal-view__input')) : []
    const inspectorStops = tab ? [tab, ...terminalStops] : terminalStops
    const sidebarStop = sidebarHidden() ? undefined
      : sidebar?.querySelector<HTMLElement>('.project[aria-current="true"], .sidebar__settings')
    const chatStop = settingsOpen() ? document.querySelector<HTMLElement>('.settings__nav button[aria-current="true"]')
      : promptInput?.disabled ? transcript : promptInput
    const stops = terminalStops.length && inspectorStops.some((stop) => stop === target) ? inspectorStops
      : [sidebarStop, chatStop, ...inspectorStops].filter((stop): stop is HTMLElement => !!stop)
    if (!stops.length) return
    const current = stops.findIndex((stop) => stop === target || (stop === sidebarStop && target instanceof Node && sidebar?.contains(target)))
    const next = current < 0 ? (step > 0 ? 0 : stops.length - 1) : (current + step + stops.length) % stops.length
    stops[next]?.focus({ preventScroll: true })
  }

  function runWorkspaceCommand(id: string, target: unknown, shortcut: boolean) {
    const project = selectedProject()
    const task = workspace()?.task
    const terminal = terminalCommands()?.actions
    if (terminal && Object.hasOwn(terminal, id)) {
      terminal[id as keyof typeof terminal]()
      return
    }
    switch (id) {
      case 'commandPalette': openPalette('commands'); break
      case 'switchProject': openPalette('projects'); break
      case 'switchTask': openPalette('tasks'); break
      case 'searchTasks': openTaskSearch(); break
      case 'addProject': openPicker(); break
      case 'settings': setProjectSettingsRequest(undefined); setSettingsOpen(true); break
      case 'toggleSidebar': toggleSidebar(); break
      case 'nextPane': focusPane(target, 1); break
      case 'previousPane': focusPane(target, -1); break
      case 'newTask': if (project) newTask(project.id); break
      case 'sendPrompt': sendPrompt(); break
      case 'cancelTurn': cancelTurn(); break
      case 'review': setInspectorTab('review'); break
      case 'terminals': setInspectorTab('terminals'); break
      case 'git': setInspectorTab('git'); break
      case 'preview': setInspectorTab('preview'); break
      case 'files': setInspectorTab('files'); break
      case 'forwardKey':
        if (workspaceTerminal(target)) {
          forwardNextKeyTo = target
          target.focus({ preventScroll: true })
        }
        break
      case 'stageAll':
      case 'commit':
        if (shortcut && id === 'commit' && target instanceof HTMLElement) {
          const form = target.closest<HTMLFormElement>('[data-git-commit]')
          const submitter = form?.querySelector<HTMLButtonElement>('button[type="submit"]:enabled')
          if (submitter) form?.requestSubmit(submitter)
        } else if (task) {
          setInspectorTab('git')
          setGitRequest({ taskId: task.id, action: id })
        }
        break
      case 'diagnostics':
        setSettingsOpen(false)
        if (diagnostics) {
          diagnostics.open = true
          diagnostics.querySelector('summary')?.focus()
        }
        break
      case 'checkAgents':
        void bridge.agentAvailability().then((available) => {
          setCommandNotice([
            `Codex ${available.codex ? 'detected' : 'unavailable'}`,
            `OpenCode ${available.opencode ? 'configured' : 'unconfigured'}`,
            `Pi ${available.pi ? 'configured' : 'unconfigured'}`,
            `Claude Code ${available.claude ? 'configured' : 'unconfigured'}`,
          ].join(' · '))
        }).catch((failure) => setError(describe(failure)))
        break
    }
  }

  function onWorkspaceKeyDown(event: KeyboardEvent) {
    if (event.defaultPrevented || (event.target instanceof HTMLElement && event.target.hasAttribute('data-shortcut-capture'))) return
    const chord = normalizeChord(event)
    if (!chord) return
    const key = event.code || event.key
    if (forwardedKey === key && event.repeat) return
    if (forwardNextKeyTo) {
      const forward = event.target === forwardNextKeyTo
      forwardNextKeyTo = undefined
      if (forward) {
        forwardedKey = key
        return
      }
    }
    if (freebuffProjectId() || pickerOpen() || searchOpen() || quitRequired() || archiveConfirmation()) return
    if (palette()) {
      if (chord === chordFor('commandPalette')) {
        event.preventDefault()
        if (!event.repeat) closePalette()
      }
      return
    }
    const target = event.target
    const scope: bridge.ShortcutScope = target === promptInput ? 'composer'
      : workspaceTerminal(target) && !target.closest('[data-agent-terminal]') ? 'workspaceTerminal'
        : target instanceof HTMLElement && target.closest('[data-git-commit]') ? 'commitForm' : 'window'
    const id = commandRegistry()?.chords[scope][chord]
    const binding = id ? bindingsById().get(id) : undefined
    if (!binding || disabledReason(binding, commandConditions(target))) return
    event.preventDefault()
    if (!event.repeat) runWorkspaceCommand(binding.id, target, true)
  }

  function onWorkspaceKeyUp(event: KeyboardEvent) {
    if ((event.code || event.key) === forwardedKey) forwardedKey = undefined
  }

  createEffect(() => {
    if (!mac || !isTauri() || !projectsLoaded()) return
    void getCurrentWindow().setBadgeCount(reviewCount() || undefined).catch((failure) =>
      console.warn('Could not update the Dock review count', failure))
  })

  async function run(action: () => Promise<void>) {
    setPending(true)
    setError(null)
    try {
      await action()
    } catch (failure) {
      setError(describe(failure))
    } finally {
      setPending(false)
    }
  }

  async function loadTasks(projectId: string) {
    const list = await bridge.taskList(projectId)
    if (selectedProjectId() === projectId) {
      setTasks(list.tasks)
      setLoadedTasksFor(projectId)
    }
    return list.tasks
  }

  async function toggleReviewFilter() {
    const enable = !reviewOnly()
    setReviewOnly(enable)
    setReviewSelection(new Set<string>())
    setReviewLists(new Map())
    reviewAnchor = undefined
    reviewLoad = undefined
    setLoadingReview(enable)
    if (!enable) return

    const updates = new Map<string, bridge.TaskSummary[]>()
    reviewLoad = updates
    try {
      const lists = await bridge.reviewTaskLists()
      if (reviewLoad !== updates) return
      setReviewLists(new Map([
        ...lists.map(({ projectId, tasks }) => [projectId, tasks] as const),
        ...updates,
      ]))
    } catch (failure) {
      if (reviewLoad !== updates) return
      setReviewOnly(false)
      setError(describe(failure))
    } finally {
      if (reviewLoad === updates) {
        reviewLoad = undefined
        setLoadingReview(false)
        reconcileReviewSelection()
      }
    }
  }

  function selectReviewTask(taskId: string, extend: boolean) {
    const ordered = reviewRows().map((task) => task.id)
    const anchor = reviewAnchor ? ordered.indexOf(reviewAnchor) : -1
    const target = ordered.indexOf(taskId)
    if (target < 0) return
    const selection = new Set(reviewSelection())
    if (extend && anchor >= 0) {
      for (const id of ordered.slice(Math.min(anchor, target), Math.max(anchor, target) + 1)) selection.add(id)
    } else {
      if (!selection.delete(taskId)) selection.add(taskId)
      reviewAnchor = taskId
    }
    setReviewSelection(selection)
  }

  function dismissArchiveConfirmation() {
    setArchiveConfirmation(undefined)
    if (archiveSelectedButton?.disabled === false) archiveSelectedButton.focus({ preventScroll: true })
    else reviewCountButton?.focus({ preventScroll: true })
  }

  function reconcileReviewSelection() {
    if (!reviewOnly() || loadingReview()) return
    const eligible = new Set(reviewRows().map((task) => task.id))
    setReviewSelection((current) => {
      const selection = new Set([...current].filter((id) => eligible.has(id)))
      return selection.size === current.size ? current : selection
    })
    if (reviewAnchor && !eligible.has(reviewAnchor)) reviewAnchor = undefined
    if (archiveConfirmation()?.some((id) => !eligible.has(id))) {
      dismissArchiveConfirmation()
      setError('The review selection changed. Check the remaining tasks and confirm again.')
    }
  }

  function archiveSelectedTasks() {
    const ids = archiveConfirmation()
    setArchiveConfirmation(undefined)
    reviewCountButton?.focus({ preventScroll: true })
    if (!ids?.length) return
    void run(async () => {
      await bridge.archiveReviewed(ids)
      setReviewSelection(new Set<string>())
      reviewAnchor = undefined
    })
  }

  async function selectTask(taskId: string) {
    setSearchJump(undefined)
    setSelectedTaskId(taskId)
    setWorkspace(null)
    const next = await bridge.taskWorkspace(taskId)
    if (selectedTaskId() === taskId) setWorkspace(next)
  }

  async function selectProject(project: bridge.ProjectSnapshot, taskId?: string) {
    setFreebuffProjectId(undefined)
    setSearchJump(undefined)
    if (selectedProjectId() !== project.id) setPrompt('')
    setTerminalListEvent(undefined)
    setSelectedProjectId(project.id)
    setSelectedTaskId(null)
    setWorkspace(null)
    setTasks([])
    setLoadedTasksFor(undefined)
    const listed = await loadTasks(project.id)
    if (selectedProjectId() !== project.id) return
    if (taskId) {
      if (!listed.some((task) => task.id === taskId)) throw new Error('This task is no longer available.')
      await selectTask(taskId)
      return
    }
    if (!project.missing && project.lastTaskId && !listed.some((task) => task.id === project.lastTaskId)) {
      await firstDraft(project.id)
      await loadTasks(project.id)
      return
    }
    const visible = listed.filter((task) => showsArchived(project.id) || task.state !== 'done')
    const task = visible.find((entry) => entry.id === project.lastTaskId)
      ?? (visible.length === 1 ? visible[0] : undefined)
    if (task) await selectTask(task.id)
  }

  function openProject(project: bridge.ProjectSnapshot, useReviewFilter = reviewOnly()) {
    setSettingsOpen(false)
    const reviewed = useReviewFilter ? filteredReviewTasks(project.id) : []
    const target = reviewed.find((task) => task.id === project.lastTaskId) ?? reviewed[0]
    void run(() => selectProject(project, target?.id)).then(() => {
      if (selectedProjectId() === project.id && canStartTask()) promptInput?.focus()
    })
  }

  function openTaskRow(task: bridge.TaskSummary) {
    setSettingsOpen(false)
    setFreebuffProjectId(undefined)
    void run(async () => {
      if (task.projectId === selectedProjectId()) {
        await selectTask(task.id)
      } else {
        const project = projects().find((candidate) => candidate.id === task.projectId)
        if (project) await selectProject(project, task.id)
      }
    })
  }

  function openTaskSearch() {
    if (pending() || pickerOpen() || quitRequired()) return
    searchInput?.focus({ preventScroll: true })
  }

  function closeTaskSearch(restoreFocus: boolean) {
    const opener = searchOpener
    searchInput?.blur()
    if (restoreFocus) opener?.focus({ preventScroll: true })
  }

  function openSearchHit(hit: bridge.TaskSearchHit) {
    closeTaskSearch(false)
    setSettingsOpen(false)
    setFreebuffProjectId(undefined)
    void run(async () => {
      const project = projects().find((candidate) => candidate.id === hit.projectId)
      if (!project) throw new Error('This search result’s project is no longer available.')
      if (selectedProjectId() === project.id) await selectTask(hit.taskId)
      else await selectProject(project, hit.taskId)
      if (selectedTaskId() !== hit.taskId || workspace()?.task.id !== hit.taskId) return
      if (!workspace()?.transcript.some((entry) => entry.id === hit.entryId)) {
        transcript?.focus({ preventScroll: true })
        throw new Error('This transcript entry is no longer available.')
      }
      setSearchJump(hit)
    })
  }

  createEffect(() => {
    const jump = searchJump()
    if (!jump || selectedTaskId() !== jump.taskId) return
    queueMicrotask(() => {
      if (searchJump() !== jump || selectedTaskId() !== jump.taskId) return
      const entry = transcript?.querySelector<HTMLElement>(`[data-entry-id="${CSS.escape(jump.entryId)}"]`)
      entry?.scrollIntoView({ block: 'center' })
      entry?.focus({ preventScroll: true })
    })
  })

  createEffect(() => {
    if (pending() || firstDrafts.has(selectedProjectId() ?? '') || searchJump()?.taskId === selectedTaskId()) return
    const visible = visibleTasks()
    const [only] = visible
    if (visible.length === 1 && only && only.id !== selectedTaskId()) {
      void run(() => selectTask(only.id))
    }
  })

  let resyncRunning = false
  let resyncRequested = false
  let resyncUpdates: {
    projects: boolean
    taskLists: Map<string, bridge.TaskSummary[]>
    workspaces: Set<string>
    terminalLists: Set<string>
  } | undefined

  function loadResyncSnapshots(projectId: string | null, reviewing: boolean) {
    return Promise.allSettled([
      bridge.projectList(),
      projectId ? bridge.taskList(projectId) : undefined,
      projectId ? bridge.terminalList(projectId) : undefined,
      reviewing ? bridge.reviewTaskLists() : undefined,
    ])
  }

  function clearMissingTask(taskId: string, projectId: string | null, tasks: bridge.TaskSummary[]) {
    setSearchJump(undefined)
    clearTurnOptions(taskId)
    clearPromptFiles(taskId)
    batch(() => {
      setTasks(tasks)
      if (projectId) setLoadedTasksFor(projectId)
      setSelectedTaskId(null)
      setWorkspace(null)
    })
  }

  async function resyncSnapshots() {
    if (resyncRunning) {
      resyncRequested = true
      return
    }
    resyncRunning = true
    try {
      do {
        resyncRequested = false
        const updates = {
          projects: false,
          taskLists: new Map<string, bridge.TaskSummary[]>(),
          workspaces: new Set<string>(),
          terminalLists: new Set<string>(),
        }
        resyncUpdates = updates
        const projectId = selectedProjectId()
        const taskId = selectedTaskId()
        const reviewing = reviewOnly()
        let snapshots: Awaited<ReturnType<typeof loadResyncSnapshots>>
        try {
          snapshots = await loadResyncSnapshots(projectId, reviewing)
        } catch (failure) {
          if (resyncRequested || projectId !== selectedProjectId() ||
              taskId !== selectedTaskId() || reviewing !== reviewOnly()) {
            resyncRequested = true
            continue
          }
          throw failure
        }
        const [projectResult, taskResult, terminalResult, reviewResult] = snapshots
        const failures: string[] = []
        function snapshotValue<T>(result: PromiseSettledResult<T>, label: string): T | undefined {
          if (result.status === 'fulfilled') return result.value
          failures.push(`${label}: ${describe(result.reason)}`)
          return undefined
        }
        const projectList = snapshotValue(projectResult, 'Projects')
        const taskList = snapshotValue(taskResult, 'Tasks')
        const terminalList = snapshotValue(terminalResult, 'Terminals')
        const reviewLists = snapshotValue(reviewResult, 'Review')
        if (resyncRequested || projectId !== selectedProjectId() ||
            taskId !== selectedTaskId() || reviewing !== reviewOnly()) {
          resyncRequested = true
          continue
        }
        const currentTasks = projectId ? updates.taskLists.get(projectId) ?? taskList?.tasks : undefined
        if (taskId && currentTasks && !currentTasks.some((task) => task.id === taskId) && !updates.workspaces.has(taskId)) {
          clearMissingTask(taskId, projectId, currentTasks)
          resyncRequested = true
          continue
        }
        let taskWorkspace: bridge.TaskWorkspace | undefined
        let workspaceFailure: { error: unknown } | undefined
        try {
          taskWorkspace = taskId ? await bridge.taskWorkspace(taskId) : undefined
        } catch (failure) {
          const latestTasks = projectId ? updates.taskLists.get(projectId) : undefined
          if (resyncRequested || projectId !== selectedProjectId() || taskId !== selectedTaskId() ||
              reviewing !== reviewOnly() || (taskId && latestTasks && !latestTasks.some((task) => task.id === taskId))) {
            resyncRequested = true
            continue
          }
          if (!taskId || !updates.workspaces.has(taskId)) workspaceFailure = { error: failure }
        }
        if (resyncRequested || projectId !== selectedProjectId() ||
            taskId !== selectedTaskId() || reviewing !== reviewOnly()) {
          resyncRequested = true
          continue
        }
        const latestTasks = projectId ? updates.taskLists.get(projectId) : undefined
        if (taskId && latestTasks && !latestTasks.some((task) => task.id === taskId) && !updates.workspaces.has(taskId)) {
          clearMissingTask(taskId, projectId, latestTasks)
          resyncRequested = true
          continue
        }
        if (projectList && !updates.projects) setProjects(projectList.projects)
        if (projectId && (taskList || updates.taskLists.has(projectId))) {
          if (taskList && !updates.taskLists.has(projectId)) setTasks(taskList.tasks)
          setLoadedTasksFor(projectId)
        }
        const settingsProjectId = settingsProject()?.id
        if (settingsProjectId) setApprovalChange({ projectId: settingsProjectId })
        if (taskWorkspace && taskId && !updates.workspaces.has(taskId)) setWorkspace(taskWorkspace)
        if (terminalList && projectId && !updates.terminalLists.has(projectId)) setTerminalListEvent(terminalList)
        if (reviewLists) {
          setReviewLists(new Map([
            ...reviewLists.map(({ projectId, tasks }) => [projectId, tasks] as const),
            ...[...updates.taskLists].map(([projectId, tasks]) => [projectId, tasks.filter((task) => task.state === 'review')] as const),
          ]))
          reconcileReviewSelection()
        }
        if (workspaceFailure) failures.push(`Workspace: ${describe(workspaceFailure.error)}`)
        if (failures.length) setError(`Could not refresh after missed updates: ${failures.join('; ')}`)
      } while (resyncRequested)
    } catch (failure) {
      setError(`Could not refresh after missed updates: ${describe(failure)}`)
    } finally {
      resyncUpdates = undefined
      resyncRunning = false
    }
  }

  function handleEvent(event: CoreEvent) {
    if (resyncUpdates) {
      switch (event.type) {
        case 'projectListChanged': resyncUpdates.projects = true; break
        case 'taskListChanged': {
          resyncUpdates.taskLists.set(event.data.projectId, event.data.tasks)
          const taskId = selectedTaskId()
          if (taskId && event.data.projectId === selectedProjectId() && !event.data.tasks.some((task) => task.id === taskId)) {
            resyncUpdates.workspaces.delete(taskId)
          }
          break
        }
        case 'taskWorkspaceChanged': resyncUpdates.workspaces.add(event.data.task.id); break
        case 'terminalListChanged': resyncUpdates.terminalLists.add(event.data.projectId); break
      }
    }
    switch (event.type) {
      case 'resyncRequired':
        void resyncSnapshots()
        break
      case 'projectListChanged':
        setProjects(event.data.projects)
        reconcileReviewSelection()
        break
      case 'projectApprovalsChanged':
        setApprovalChange(event.data)
        break
      case 'taskListChanged': {
        const { projectId, tasks } = event.data
        if (projectId === selectedProjectId()) setTasks(tasks)
        if (reviewOnly()) {
          const reviewed = tasks.filter((task) => task.state === 'review')
          reviewLoad?.set(projectId, reviewed)
          setReviewLists((lists) => new Map(lists).set(projectId, reviewed))
          reconcileReviewSelection()
        }
        break
      }
      case 'taskWorkspaceChanged':
        if (event.data.task.id === selectedTaskId()) setWorkspace(event.data)
        break
      case 'taskNeedsInput': {
        const { taskId } = event.data
        const focused = taskId === selectedTaskId() && document.hasFocus()
          && !settingsOpen() && !freebuffProjectId() && !searchOpen() && !pickerOpen()
        if (notificationsEnabled() && !focused) {
          void bridge.notifyTaskNeedsInput(taskId).catch((failure) => setError(describe(failure)))
        }
        break
      }
      case 'terminalBell':
        if (event.data.terminalId === focusedTerminal() && inspectorTab() === 'terminals' && document.hasFocus()) break
        setTerminalAttention((bells) => bells.some((bell) => bell.terminalId === event.data.terminalId) ? bells : [...bells, event.data])
        break
      case 'terminalListChanged':
        if (event.data.projectId === selectedProjectId()) {
          setTerminalListEvent((current) => current?.projectId === event.data.projectId &&
            current.revision >= event.data.revision ? current : event.data)
        }
        break
      case 'error':
        setError(event.data.message)
        break
      case 'shutdownStateChanged':
        if (event.data === 'closed') setShutdownNote('core stopped')
        break
    }
  }

  onMount(() => {
    void inspectPreviousProcesses()
    void bridge.archivedWorktreeIssues().then(setWorktreeIssues).catch((failure) => setError(describe(failure)))
    void homeDir().then(setHomeDirectory).catch((error) => console.warn('Could not resolve the home directory', error))
    void reloadKeybindings().catch((failure) => setError(describe(failure)))
    void bridge.opencodeConfigured().then(setOpencodeConfigured).catch((error) => console.warn('Could not read OpenCode availability', error))
    void bridge.piConfigured().then(setPiConfigured).catch((error) => console.warn('Could not read Pi availability', error))
    void bridge.claudeConfigured().then(setClaudeConfigured).catch((error) => console.warn('Could not read Claude Code availability', error))
    void run(async () => {
      await reloadPreferences().catch((failure) => setError(describe(failure)))
      await bridge.subscribeToEvents(handleEvent)
      const list = await bridge.projectList()
      setProjects(list.projects)
      setProjectsLoaded(true)
      if (list.projects[0]) await selectProject(list.projects[0])
      setCodexAvailable(await bridge.localCodexAvailable())
    })
  })

  async function scanProjects(rescan: boolean) {
    setScanning(true)
    setScanError(null)
    try {
      setScan(await bridge.scanProjects(rescan))
    } catch (failure) {
      setScanError(describe(failure))
    } finally {
      setScanning(false)
    }
  }

  function openPicker() {
    setPickerOpen(true)
    if (!scan() && !scanning()) void scanProjects(false)
  }

  onMount(() => {
    let disposed = false
    let detach: (() => void) | undefined
    window.addEventListener('keydown', onWorkspaceKeyDown, true)
    window.addEventListener('keyup', onWorkspaceKeyUp, true)
    window.addEventListener('keydown', onInspectorFocusShortcut, true)
    void getCurrentWindow().onCloseRequested((event) => {
      if (allowClose) return
      event.preventDefault()
      if (!pending()) quit()
    }).then((unlisten) => {
      if (disposed) unlisten()
      else detach = unlisten
    }).catch(console.error)
    onCleanup(() => {
      disposed = true
      detach?.()
      window.removeEventListener('keydown', onWorkspaceKeyDown, true)
      window.removeEventListener('keyup', onWorkspaceKeyUp, true)
      window.removeEventListener('keydown', onInspectorFocusShortcut, true)
    })
  })

  function addProject(path: string) {
    setPickerOpen(false)
    void run(() => addProjects([path]))
  }

  function importProjects() {
    if (pending() || !codexAvailable()) return
    const fromSettings = settingsOpen()
    setImportNote(null)
    void run(async () => {
      const available = await bridge.localCodexProjects()
      const registered = new Set(projects().map((project) => project.path))
      const paths = available.map((project) => project.path).filter((path) => !registered.has(path))
      if (paths.length === 0) {
        setImportNote('No new local Codex project folders to import.')
        return
      }
      await addProjects(paths, !fromSettings)
    })
  }

  async function addProjects(paths: string[], selectAdded = true) {
    let target: bridge.ProjectSnapshot | undefined
    const errors: string[] = []
    for (const path of paths) {
      try {
        const list = await bridge.addProject(path)
        setProjects(list.projects)
        target ??= list.projects.find((project) => project.path === path) ?? list.projects.at(-1)
      } catch (failure) {
        errors.push(describe(failure))
      }
    }
    if (target && (selectAdded || !selectedProjectId())) {
      if (selectAdded) setSettingsOpen(false)
      await selectProject(target)
    }
    if (errors.length) setError(errors.join(' · '))
  }

  function newTask(projectId: string) {
    setFreebuffProjectId(undefined)
    setSettingsOpen(false)
    void run(async () => {
      const draft = await bridge.createDraft(projectId, 'New task', newTaskAgent())
      moveTurnOptionsToDraft(projectId, draft.task.id)
      setPrompt('')
      if (selectedProjectId() !== projectId) {
        setTerminalListEvent(undefined)
        setSelectedProjectId(projectId)
        setTasks([])
        setLoadedTasksFor(undefined)
      }
      setSelectedTaskId(draft.task.id)
      setWorkspace(draft)
      await loadTasks(projectId)
    })
  }

  function firstDraft(projectId: string) {
    const inFlight = firstDrafts.get(projectId)
    if (inFlight) return inFlight
    const request = bridge.createDraft(projectId, 'New task', newTaskAgent()).then((draft) => {
      moveTurnOptionsToDraft(projectId, draft.task.id)
      setQueuedFiles((queues) => {
        const files = queues.get(`project:${projectId}`)
        if (!files) return queues
        const next = new Map(queues)
        next.delete(`project:${projectId}`)
        return next.set(`task:${draft.task.id}`, files)
      })
      if (selectedProjectId() === projectId && !selectedTaskId()) {
        setSelectedTaskId(draft.task.id)
        setWorkspace(draft)
      }
      return draft
    }).finally(() => firstDrafts.delete(projectId))
    firstDrafts.set(projectId, request)
    return request
  }

  function updatePrompt(text: string) {
    setPrompt(text)
    const projectId = selectedProjectId()
    if (!projectId || !text.trim() || pending() || !canStartTask() || firstDrafts.has(projectId)) return
    void firstDraft(projectId).then(() => loadTasks(projectId)).catch((failure) => {
      if (selectedProjectId() === projectId) setError(describe(failure))
    })
  }

  function queuePromptFiles(files: FileList) {
    const key = fileQueueKey()
    const limit = attachmentLimit()
    if (!key || !limit || !canCompose() || pending()) return
    const accepted: File[] = []
    const oversized: string[] = []
    for (const file of files) {
      if (file.size > limit.bytes) oversized.push(file.name)
      else accepted.push(file)
    }
    if (accepted.length) {
      setQueuedFiles((queues) => new Map(queues).set(key, [...(queues.get(key) ?? []), ...accepted]))
    }
    setError(oversized.length ? `Attachments must be up to ${limit.label} each. ${oversized.join(', ')}` : null)
  }

  function removePromptFile(file: File) {
    const key = fileQueueKey()
    if (key) setQueuedFiles((queues) => new Map(queues).set(key, (queues.get(key) ?? []).filter((queued) => queued !== file)))
  }

  function clearPromptFiles(taskId: string) {
    setQueuedFiles((queues) => {
      const next = new Map(queues)
      next.delete(`task:${taskId}`)
      return next
    })
  }

  function appendPreviewAnnotations(annotations: unknown[]) {
    const text = `Preview annotations\n${JSON.stringify(annotations, null, 2)}`
    const current = prompt().trimEnd()
    updatePrompt(current ? `${current}\n\n${text}` : text)
    promptInput?.focus()
  }

  async function addPreviewNote(annotations: PreviewAnnotation[]) {
    const projectId = selectedProjectId()
    if (!projectId || !canAddPreviewNote()) throw new Error('Select a draft or ready task to add annotations.')
    if (!annotations.some((annotation) => 'screenshot' in annotation)) {
      appendPreviewAnnotations(annotations)
      return
    }

    const context: unknown[] = []
    setPending(true)
    try {
      const task = workspace()?.task ?? (await firstDraft(projectId)).task
      for (const annotation of annotations) {
        if ('screenshot' in annotation) {
          const { file, ...coordinates } = annotation.screenshot
          const path = await bridge.savePreviewScreenshot(task.id, file)
          context.push({ url: annotation.url, screenshot: { path, ...coordinates }, note: annotation.note })
        } else {
          context.push(annotation)
        }
      }
      if (selectedProjectId() !== projectId || selectedTaskId() !== task.id) {
        throw new Error('The selected task changed. Add the annotations again from the intended task.')
      }
    } finally {
      setPending(false)
    }
    appendPreviewAnnotations(context)
  }

  function discardDraft() {
    const current = workspace()
    if (!current) return
    void run(async () => {
      await bridge.discardDraft(current.task.id)
      clearTurnOptions(current.task.id)
      clearPromptFiles(current.task.id)
      setWorkspace(null)
      setSelectedTaskId(null)
      const projectId = selectedProjectId()
      if (projectId) await loadTasks(projectId)
    })
  }

  function deleteTask(removeWorktree: boolean) {
    const task = deleteRequest()
    if (!task || pending()) return
    void run(async () => {
      await bridge.deleteTask(task.id, removeWorktree)
      clearTurnOptions(task.id)
      clearPromptFiles(task.id)
      setDeleteRequest(undefined)
      if (selectedTaskId() === task.id) {
        setWorkspace(null)
        setSelectedTaskId(null)
      }
      await loadTasks(task.projectId)
    })
  }

  function sendPrompt() {
    const projectId = selectedProjectId()
    const current = workspace()
    const text = prompt().trim()
    const files = promptFiles()
    const codex = currentAgent() === 'codex' ? codexOptions() : undefined
    const mode = pendingMode()
    if (!projectId || (!text && !files.length) || !canCompose() || pending()) return
    void run(async () => {
      const task = current?.task ?? (await firstDraft(projectId)).task
      const attachmentIds: string[] = []
      for (const file of files) attachmentIds.push(await bridge.savePromptFile(task.id, file))
      const updated = await bridge.sendPrompt(task.id, text, attachmentIds, codex, mode)
      clearPromptFiles(task.id)
      clearTurnOptions(task.id)
      if (selectedTaskId() === task.id) {
        setPrompt((value) => value.trim() === text ? '' : value)
        setWorkspace(updated)
      }
      await loadTasks(projectId)
    })
  }

  function chooseMode(mode: string) {
    const current = workspace()
    if (pending()) return
    if (!current?.modes) {
      setTurnOptions({ mode: mode || undefined })
      return
    }
    const taskId = current.task.id
    if (!mode) {
      clearTurnOptions(taskId)
      return
    }
    void run(async () => {
      const updated = await bridge.setSessionMode(taskId, mode)
      clearTurnOptions(taskId)
      if (selectedTaskId() === taskId) setWorkspace(updated)
    })
  }

  function decide(approvalId: string, decision: bridge.Decision) {
    const current = workspace()
    if (!current || !canApprove() || cancellingTurn()) return
    void run(async () => {
      setWorkspace(await bridge.resolveApproval(current.task.id, approvalId, decision))
      const projectId = selectedProjectId()
      if (projectId) await loadTasks(projectId)
    })
  }

  function cancelTurn() {
    const current = workspace()
    if (!current || !canCancel() || current.cancelling || cancelRequestTaskId()) return
    setError(null)
    setCancelRequestTaskId(current.task.id)
    void bridge.cancelTask(current.task.id).then((updated) => {
      // A newer snapshot from the event channel may already show the turn ended.
      setWorkspace((previous) => previous === current ? updated : previous)
    }).catch((failure) => {
      setError(describe(failure))
    }).finally(() => {
      setCancelRequestTaskId(undefined)
    })
  }

  function onInspectorFocusShortcut(event: KeyboardEvent) {
    if (event.defaultPrevented || event.isComposing || event.keyCode === 229 || event.altKey ||
        !workspaceShortcutsEnabled() ||
        inspectorTab() !== 'terminals' || !inspector) return
    const tab = inspectorTabList?.querySelector<HTMLButtonElement>('#inspector-tab-terminals')
    const target = event.target
    if (!tab || !(target instanceof HTMLElement) || (target !== tab && !inspector.contains(target))) return
    if (event.ctrlKey || event.metaKey || event.shiftKey ||
        !['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight'].includes(event.key)) return
    const destination = terminalArrowDestination(inspector, tab, target, event.key)
    if (destination) {
      event.preventDefault()
      destination.focus()
    }
  }

  function onInspectorTabKeyDown(event: KeyboardEvent, tabList: HTMLDivElement) {
    if (event.defaultPrevented || event.isComposing || event.altKey || event.ctrlKey || event.metaKey ||
        !['ArrowRight', 'ArrowLeft', 'Home', 'End'].includes(event.key)) return
    const tabs = Array.from(tabList.querySelectorAll<HTMLButtonElement>('button:enabled'))
    const current = tabs.findIndex((tab) => tab === event.target)
    if (current < 0) return
    event.preventDefault()
    const step = event.key === 'ArrowLeft' ? -1 : 1
    let next = (current + step + tabs.length) % tabs.length
    if (event.key === 'Home') next = 0
    if (event.key === 'End') next = tabs.length - 1
    tabs[next]?.click()
    tabs[next]?.focus()
  }

  function resumeTask() {
    const current = workspace()
    if (!current?.recovery || pending()) return
    const task = current.task
    void run(async () => {
      const resumed = await bridge.resumeTask(task.id)
      if (selectedTaskId() === task.id) setWorkspace(resumed)
      await loadTasks(task.projectId)
    })
  }

  function quit() {
    void run(async () => {
      try {
        await pendingInspectorTabSave
        const summary = await bridge.quit(false)
        setShutdownNote(`quit: ${summary.interruptedTasks} interrupted`)
        allowClose = true
        await getCurrentWindow().close()
      } catch (failure) {
        const confirmation = (failure as bridge.BridgeError | null)?.quitConfirmation
        if (confirmation) {
          setQuitRequired(confirmation)
        } else {
          setError(describe(failure))
        }
      }
    })
  }

  function forceQuit() {
    void run(async () => {
      await pendingInspectorTabSave
      const summary = await bridge.quit(true)
      setQuitRequired(null)
      setShutdownNote(`quit: ${summary.interruptedTasks} interrupted`)
      allowClose = true
      await getCurrentWindow().close()
    })
  }

  const needsReview = () => workspace()?.task.state === 'review'
  const terminalAttentionId = createUniqueId()
  const reviewAttentionId = createUniqueId()
  const inspectorTabsHeader = (
    <div
      class="inspector__tabs"
      role="tablist"
      ref={inspectorTabList}
      aria-label="Inspector tabs"
      onKeyDown={(event) => onInspectorTabKeyDown(event, event.currentTarget)}
    >
      <For each={INSPECTOR_TABS}>
        {(tab) => (
          <button
            type="button"
            role="tab"
            class="shell__icon-button inspector__tab"
            id={`inspector-tab-${tab.id}`}
            aria-label={tab.label}
            aria-describedby={
              tab.id === 'terminals' && tabAttention() ? terminalAttentionId
                : tab.id === 'review' && needsReview() ? reviewAttentionId : undefined
            }
            aria-selected={tab.id === inspectorTab()}
            tabIndex={tab.id === inspectorTab() ? 0 : -1}
            onClick={() => setInspectorTab(tab.id)}
            aria-disabled={!tab.enabled}
            aria-controls={`inspector-panel-${tab.id}`}
            disabled={!tab.enabled}
            title={tab.enabled ? tab.label : `${tab.label} arrives in a later slice`}
          >
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d={tab.icon} />
            </svg>
            <Show when={tab.id === 'terminals' && tabAttention()}>
              <span id={terminalAttentionId} class="terminal-attention" role="img" aria-label="Terminal needs attention" />
            </Show>
            <Show when={tab.id === 'review' && needsReview()}>
              <span id={reviewAttentionId} class="terminal-attention" role="img" aria-label="Needs review" />
            </Show>
          </button>
        )}
      </For>
    </div>
  )

  const workspaceFooter = (
    <>
      <div class="shell__notices">
        <Show when={!worktreeNoticeDismissed() && worktreeIssues().length > 0}>
          <div class="banner" role="status">
            <details class="process-notice__details">
              <summary>Archived worktree collection needs attention ({worktreeIssues().length})</summary>
              <ul>
                <For each={worktreeIssues()}>{(issue) => <li>{issue.title} · {issue.path} · {issue.reason}</li>}</For>
              </ul>
            </details>
            <button type="button" class="banner__quiet" onClick={() => setWorktreeNoticeDismissed(true)}>Dismiss</button>
          </div>
        </Show>
        <Show when={!processNoticeDismissed() && processesNeedingReview().length > 0}>
          <div class="banner" role="status">
            <details class="process-notice__details">
              <summary>Previously recorded processes need review ({processesNeedingReview().length})</summary>
              <p>A matching process may belong to another running Shepherd instance. Shepherd stops one only when no running instance owns its lease and its identity still matches.</p>
              <ul>
                <For each={processesNeedingReview()}>{(process) =>
                  <li>
                    {process.kind === 'agent' ? 'Agent' : process.kind === 'acp_terminal' ? 'Agent terminal' : 'Terminal'} · PID {process.pid} · {process.status === 'matching' ? 'identity matches' : process.status === 'changed' ? 'process group or session is unsafe' : process.status === 'groupUnverified' ? 'leader exited; process group may still be active' : 'identity could not be verified'}
                    <Show when={process.status === 'matching'}>
                      <button type="button" disabled={stoppingProcess() === process.id} onClick={() => void stopPreviousProcess(process.id)}>Stop</button>
                    </Show>
                  </li>
                }</For>
              </ul>
            </details>
            <button type="button" disabled={checkingProcesses()} onClick={() => void inspectPreviousProcesses()}>Recheck</button>
            <button type="button" class="banner__quiet" onClick={() => setProcessNoticeDismissed(true)}>Dismiss</button>
          </div>
        </Show>
        <Show when={preferences()?.warning}>
          {(warning) => <div class="banner banner--error" role="alert"><span>{warning()} Default settings are active.</span></div>}
        </Show>
        <Show when={commandRegistry()?.warning}>
          {(warning) => <div class="banner banner--error" role="alert"><span>{warning()} Default shortcuts are active.</span></div>}
        </Show>
        <Show when={commandNotice()}>
          {(notice) => <div class="banner" role="status">
            <span>{notice()}</span>
            <button type="button" class="banner__quiet" onClick={() => setCommandNotice('')}>Dismiss</button>
          </div>}
        </Show>
        <Show when={quitRequired()}>
          {(confirmation) => (
            <div class="banner" role="alert">
              <span>
                Quit will stop {confirmation().liveTasks} live task{confirmation().liveTasks === 1 ? '' : 's'}
                <Show when={confirmation().liveTerminals}>
                  {(count) => ` and ${count()} terminal${count() === 1 ? '' : 's'}`}
                </Show>
                . Quit anyway?
              </span>
              <button type="button" onClick={forceQuit} disabled={pending()}>
                Quit anyway
              </button>
              <button type="button" class="banner__quiet" onClick={() => setQuitRequired(null)}>
                Keep working
              </button>
            </div>
          )}
        </Show>

      </div>
      <footer class="status-bar" aria-label="Status">
        <ErrorNotifications />
        <span>{agentActivity(workspace())}</span>
        <Dot />
        <span>{runningCount()} {runningCount() === 1 ? 'task' : 'tasks'} running</span>
        <Dot />
        <button type="button" class="status-bar__filter" ref={reviewCountButton} aria-pressed={reviewOnly()}
          title="Filter projects to tasks awaiting review" onClick={() => void toggleReviewFilter()}>
          {reviewCount()} in review
        </button>
        <Show when={worktreePath()}>
          <Dot />
          <Show when={worktreeGit()} fallback={<span title={gitStatusError()}>{gitStatusError() ? 'Git status unavailable' : 'Reading Git status…'}</span>}>
            {(git) => <>
              <span class="status-bar__branch" title={git().branch ?? 'Detached HEAD'}>
                {git().branch ? `branch ${git().branch}` : 'Detached HEAD'}
              </span>
              <Dot />
              <span>{git().changeCount} {git().changeCount === 1 ? 'changed file' : 'changed files'}</span>
            </>}
          </Show>
        </Show>
        <Show when={shutdownNote()}>
          {(note) => (
            <>
              <Dot />
              <span>{note()}</span>
            </>
          )}
        </Show>
      </footer>
    </>
  )

  return (
    <>
      <WorkspaceFrame taskId={selectedTaskId()} inspectorWidth={inspectorWidth()} onInspectorWidthChange={resizeInspector}
        search={<TaskSearch open={searchOpen()} disabled={pending() || !projects().length} shortcut={chordFor('searchTasks')}
          mac={mac}
          inputRef={(input) => { searchInput = input }}
          onFocus={(opener) => {
            searchOpener = opener instanceof HTMLElement ? opener : undefined
            setSearchOpen(true)
          }}
          onBlur={() => { setSearchOpen(false); searchOpener = undefined }}
          onEscape={() => closeTaskSearch(true)} onSelect={openSearchHit} />}
        sidebarHidden={sidebarHidden()} onCommands={() => openPalette('commands')} paletteShortcut={chordFor('commandPalette')}
        footer={workspaceFooter}
        onBackToChat={freebuffProjectId() && !settingsOpen() ? () => setFreebuffProjectId(undefined) : undefined}>
        <aside ref={sidebar} class="sidebar" aria-label="Workspace navigation" hidden={sidebarHidden()}>
          <header class="sidebar__header">
            <h2 class="sidebar__title">Projects</h2>
            <button
              type="button"
              class="project-action project-action--icon"
              aria-label="Add project"
              title="Add project"
              onClick={openPicker}
              disabled={pending()}
            >
              <FolderPlusIcon />
            </button>
          </header>

          <Show when={reviewOnly()}>
            <div class="review-selection">
              <span role="status">{loadingReview() ? 'Loading review tasks…' : selectedReviewIds().length
                ? `${selectedReviewIds().length} selected` : sidebarProjects().length ? 'Needs review' : 'No tasks in review'}</span>
              <button type="button" class="project-action" ref={archiveSelectedButton}
                disabled={pending() || loadingReview() || !selectedReviewIds().length}
                onClick={() => setArchiveConfirmation(selectedReviewIds())}>Archive selected</button>
            </div>
          </Show>

          <ul class="projects">
            <Show when={!reviewOnly() && projectsLoaded() && projects().length === 0}>
              <li class="projects__empty">
                <button type="button" class="project-action projects__new" onClick={openPicker} disabled={pending()}>
                  <PlusIcon />
                  New project
                </button>
                <p class="empty">or import all from</p>
                <ProjectImportSources codexDisabled={pending() || !codexAvailable()} note={importNote()} onImportCodex={importProjects} />
              </li>
            </Show>
            <For each={sidebarProjects()}>
              {(project) => {
                const listed = () => reviewOnly() ? filteredReviewTasks(project.id)
                  : project.id === selectedProjectId() && visibleTasks().length > 1 ? visibleTasks() : []
                return (
                  <li>
                    <ProjectRow
                      project={project}
                      homeDirectory={homeDirectory()}
                      selected={project.id === selectedProjectId()}
                      expanded={listed().length > 0}
                      pending={pending()}
                      showArchived={showsArchived(project.id)}
                      archiveDisabled={!preferencesEditable()}
                      menuOpen={projectMenuId() === project.id}
                      onSelect={() => openProject(project)}
                      onCompose={() => newTask(project.id)}
                      onMenuOpenChange={(open) => {
                        if (open) setProjectMenuId(project.id)
                        else if (projectMenuId() === project.id) setProjectMenuId(undefined)
                      }}
                      onToggleArchived={() => requestPreference('tasks.show_archived', !showsArchived(project.id), project.id)}
                      onProjectSettings={() => openProjectSettings(project.id)}
                    />
                    <Show when={project.missing}>
                      <p class="project__missing">path is missing</p>
                    </Show>

                    <Show when={listed().length}>
                      <TaskRows tasks={listed()} currentTaskId={selectedTaskId()} mac={mac} disabled={pending()}
                        selection={sidebarSelection}
                        needsTerminalAttention={(id) => terminalAttention().some((bell) => bell.taskId === id)}
                        onOpen={openTaskRow} onSelect={selectReviewTask} />
                    </Show>
                  </li>
                )
              }}
            </For>
          </ul>
          <div class="sidebar__footer">
            <button
              type="button"
              class="nav-item sidebar__settings"
              aria-pressed={settingsOpen()}
              onClick={() => {
                setProjectSettingsRequest(undefined)
                setSettingsOpen((open) => !open)
              }}
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <circle cx="12" cy="12" r="4" />
                <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.55V21a2 2 0 1 1-4 0v-.08A1.7 1.7 0 0 0 8.97 19.4a1.7 1.7 0 0 0-1.88.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.55-1H3a2 2 0 1 1 0-4h.08A1.7 1.7 0 0 0 4.6 8.97a1.7 1.7 0 0 0-.34-1.88l-.06-.06A2 2 0 1 1 7.03 4.2l.06.06A1.7 1.7 0 0 0 8.97 4.6 1.7 1.7 0 0 0 10 3.05V3a2 2 0 1 1 4 0v.08A1.7 1.7 0 0 0 15.03 4.6a1.7 1.7 0 0 0 1.88-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 8.97 1.7 1.7 0 0 0 20.95 10H21a2 2 0 1 1 0 4h-.08A1.7 1.7 0 0 0 19.4 15Z" />
              </svg>
              Settings
            </button>
          </div>
        </aside>

        <Show when={settingsOpen()}>
          <SettingsPane
            keybindings={commandRegistry()}
            mac={mac}
            onReloadKeybindings={reloadKeybindings}
            onEditKeybinding={editKeybinding}
            preferences={preferences()}
            preferencesEditable={preferencesEditable()}
            preferenceBusy={savingPreference()}
            onEditPreference={editPreference}
            onReloadPreferences={() => void reloadPreferences().catch((failure) => setError(describe(failure)))}
            settingsConflicts={settingsConflicts()}
            onRetryPreference={(edit) => requestPreference(edit.key, edit.value, edit.projectId)}
            onDiscardConflict={discardSettingsConflict}
            showArchived={showsArchived(null)}
            onShowArchived={(visible) => requestPreference('tasks.show_archived', visible)}
            notificationsEnabled={notificationsEnabled()}
            onNotificationsEnabled={(enabled) => requestPreference('notifications.enabled', enabled)}
            sendAccent={sendAccent()}
            onSendAccent={(accent) => requestPreference('appearance.send_accent', accent)}
            onAddProject={openPicker}
            pending={pending()}
            projectImports={<ProjectImportSources codexDisabled={pending() || !codexAvailable()} note={importNote()} onImportCodex={importProjects} />}
            project={settingsProject()}
            projectSettingsRequest={projectSettingsRequest()}
            approvalChange={approvalChange()}
            showProjectArchived={showsArchived(settingsProject()?.id ?? null)}
            onShowProjectArchived={(projectId, visible) => requestPreference('tasks.show_archived', visible, projectId)}
            onSavePreviewUrl={async (projectId, url) => {
              const { projects: updated } = await bridge.setProjectPreviewUrl(projectId, url)
              const project = updated.find((entry) => entry.id === projectId)
              if (project) setProjects((current) => current.map((entry) => entry.id === projectId
                ? { ...entry, previewUrl: project.previewUrl }
                : entry))
            }}
          />
        </Show>

        <Show when={freebuffProjectId()} keyed>
          {(projectId) => (
            <FreebuffPane projectId={projectId} shown={!settingsOpen()} />
          )}
        </Show>

        <section class="chat" aria-label="Task chat" hidden={settingsOpen() || !!freebuffProjectId()}>
          <Show when={workspace()}>
            {(current) => (
              <header class="pane-header">
                <span class="chat__title"><SearchMark text={current().task.title}
                  match={searchJump()?.taskId === current().task.id && searchJump()?.snippet.field === 'task' ? searchJump()?.snippet.matched : undefined} /></span>
                <Show when={current().adapterTier}>
                  {(tier) => <span class="chat__adapter">{TIER_LABELS[tier()]}{current().diagnostics?.fallbackReason && ' · fallback'}</span>}
                </Show>
                <span class="chat__state">{current().task.state}</span>
                <Show when={current().task.state !== 'draft'}>
                  <button type="button" class="project-action" disabled={pending() || LIVE_STATES.includes(current().task.state)}
                    onClick={() => setDeleteRequest(current().task)}>Delete task…</button>
                </Show>
                <Show when={canCancel()}>
                  <button type="button" class="project-action" onClick={cancelTurn}
                    aria-keyshortcuts={chordFor('cancelTurn')}
                    disabled={cancellingTurn() || !!cancelRequestTaskId()}>
                    {cancellingTurn() ? 'Cancelling…' : 'Cancel turn'}
                  </button>
                </Show>
                <Show when={current().recovery}>
                  {(recovery) => <button type="button" class="project-action" onClick={resumeTask} disabled={pending()}>
                    {RECOVERY_LABELS[recovery()]}
                  </button>}
                </Show>
              </header>
            )}
          </Show>

          <Show when={workspace()}>
            {(current) => (
              <Show when={current().task.state === 'draft'}>
                <div class="draft">
                  <p class="draft__note">
                    Nothing is created yet. The first prompt creates the branch and the
                    worktree below.
                  </p>
                  <dl class="draft__facts">
                    <dt>Proposed branch</dt>
                    <dd>
                      <code>{current().task.proposedBranch}</code>
                    </dd>
                    <dt>Proposed worktree</dt>
                    <dd>
                      <code>{current().task.proposedWorktreePath}</code>
                    </dd>
                  </dl>
                  <button type="button" onClick={discardDraft} disabled={pending()}>
                    Discard draft
                  </button>
                </div>
              </Show>
            )}
          </Show>

          <Show when={workspace()?.diagnostics?.fallbackReason}>
            {(reason) => <p class="chat__adapter-note">{reason()}</p>}
          </Show>

          <Show when={workspace()?.diagnostics}>
            {(diagnostics) => (
              <details class="diagnostics" ref={diagnostics}>
                <summary>Diagnostics</summary>
                <dl>
                  <dt>Command</dt>
                  <dd>
                    <code>{diagnostics().command || '—'}</code>
                  </dd>
                  <dt>Protocol</dt>
                  <dd>{diagnostics().protocol || '—'}</dd>
                  <Show when={workspace()?.adapterTier}>
                    {(tier) => <><dt>Tier</dt><dd>{TIER_LABELS[tier()]}</dd></>}
                  </Show>
                  <Show when={workspace()?.capabilities}>
                    {(capabilities) => <>
                      <dt>Supported features</dt>
                      <dd>{Object.entries(CAPABILITY_LABELS)
                        .filter(([key]) => capabilities()[key as keyof bridge.AgentCapabilities])
                        .map(([, label]) => label).join(', ') || 'None reported'}</dd>
                    </>}
                  </Show>
                  <Show when={diagnostics().stderrTail}>
                    <dt>stderr</dt>
                    <dd>
                      <pre>{diagnostics().stderrTail}</pre>
                    </dd>
                  </Show>
                </dl>
              </details>
            )}
          </Show>

          <div class="chat__transcript" ref={transcript} tabIndex={-1} role="region" aria-label="Transcript">
            <Show when={workspace() && selectedProject()?.missing}>
              <p class="empty" role="status">The project folder is unavailable.</p>
            </Show>
            <Show when={!workspace()?.transcript.some((entry) => entry.kind.type === 'failure') && workspace()?.task.error}>
              {(error) => <p class="entry__failure-text" role="alert">{error()}</p>}
            </Show>
            <Show
              when={workspace()}
              fallback={
                <Show when={canStartTask() && selectedProject()} fallback={
                  <div class="chat__welcome"><p class="empty" role="status">{emptyChatMessage()}</p></div>
                }>
                  {(project) => (
                    <div class="chat__welcome">
                      <h1 aria-label={`What should ${currentAgentName()} build in ${project().name}?`}>
                        What should{' '}
                        <HeadlinePicker label="Agent" value={currentAgent()} text={currentAgentName()} options={agentOptions()} onChange={chooseAgent} />
                        {' '}build in{' '}
                        <HeadlinePicker
                          label="Project"
                          value={project().id}
                          text={project().name}
                          options={projects().map(({ id, name, path, missing }) => ({ id, name, detail: path, disabled: missing }))}
                          onChange={(projectId) => {
                            const next = projects().find((candidate) => candidate.id === projectId)
                            if (next && next.id !== selectedProjectId()) openProject(next)
                          }}
                        />
                        ?
                      </h1>
                    </div>
                  )}
                </Show>
              }
            >
              {(current) => (
                <Show
                  when={current().transcript.length > 0}
                  fallback={<p class="empty">Send a prompt to start this task.</p>}
                >
                  <ol class="transcript">
                    <For each={current().transcript}>
                      {(entry) => {
                        const target = () => {
                          const jump = searchJump()
                          return jump?.taskId === selectedTaskId() && jump?.entryId === entry.id ? jump : undefined
                        }
                        return <li class="entry" classList={{ 'entry--search-hit': !!target() }}
                          data-entry-id={entry.id} tabIndex={-1}>
                          {entryBody(entry, decide, cancellingTurn, canApprove,
                            () => target()?.snippet.field === 'entry' ? target()?.snippet.matched : undefined)}
                        </li>
                      }}
                    </For>
                  </ol>
                </Show>
              )}
            </Show>
          </div>

          <div class="composer">
            <Show when={workspace()?.task.state === 'waiting'}>
              <span class="terminal-attention composer__attention" role="img" aria-label="Task needs input" />
            </Show>
            <input type="file" ref={fileInput} aria-label="Local attachments" hidden multiple
              onChange={(event) => {
                if (event.currentTarget.files) queuePromptFiles(event.currentTarget.files)
                event.currentTarget.value = ''
              }} />
            <Show when={promptFiles().length > 0}>
              <ul class="composer__files" aria-label="Queued attachments">
                <For each={promptFiles()}>{(file) => <li>
                  <span>{file.name}</span>
                  <button type="button" aria-label={`Remove attachment ${file.name}`} disabled={pending()}
                    onClick={() => removePromptFile(file)}>×</button>
                </li>}</For>
              </ul>
            </Show>
            <ComposerInput
              inputRef={(element) => { promptInput = element }}
              projectId={selectedProjectId()}
              taskId={workspace()?.task.worktreePath ? selectedTaskId() : null}
              placeholder={canCompose() ? 'Send a prompt' : 'Select a draft or ready task'}
              value={prompt()}
              disabled={!canCompose() || pending()}
              onInput={(text, composing) => {
                if (composing) setPrompt(text)
                else updatePrompt(text)
              }}
              onSend={sendPrompt}
            />
            <div class="composer__actions">
              <ComposerPickers agent={currentAgent()} agentName={currentAgentName()} agents={agentOptions()} onAgentChange={chooseAgent}
                showShellApprovals={showShellApprovals()} canAttach={canCompose() && !pending() && !!attachmentLimit()}
                onAttach={() => fileInput?.click()} />
              <div class="composer__send-actions">
                <Show when={composerModes()}>
                  {(modes) => <ComposerModePicker modes={modes()} onChange={chooseMode}
                    defaultOption={workspace()?.modes ? pendingMode() ? 'Use session mode' : undefined : 'Default mode'}
                    disabled={pending() || (!canCompose() && workspace()?.task.state !== 'review')} />}
                </Show>
                <Show when={currentAgent() === 'codex'}>
                  <ComposerModelPicker model={composerModel()} effort={composerEffort()} models={codexCatalog()?.models ?? []}
                    disabled={!canCompose() || pending() || workspace()?.capabilities?.modelPicker === false}
                    options={codexOptions()} onChange={(codex) => setTurnOptions({ codex })} />
                </Show>
                <button
                  type="button"
                  class="composer__send composer__send--icon"
                  data-send-accent={sendAccent()}
                  aria-label="Send"
                  aria-keyshortcuts={chordFor('sendPrompt')}
                  title="Send"
                  onClick={sendPrompt}
                  disabled={!canCompose() || pending() || (!prompt().trim() && !promptFiles().length)}
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M3 4.5 21 12 3 19.5l3.6-7.5L3 4.5Z M6.6 12H14" />
                  </svg>
                </button>
              </div>
            </div>
          </div>
        </section>

        <aside ref={inspector} class="inspector" id="workspace-inspector" aria-label="Inspector">
          <div class="shell__header-inspector" data-tauri-drag-region={isTauri() && mac ? '' : undefined}>
            {inspectorTabsHeader}
          </div>
          <div class="inspector__pane">
            <InspectorPanel id="terminals" active={inspectorTab()}>
              <Show when={selectedProjectId()} keyed fallback={<p class="empty">No terminals yet.</p>}>
                {(projectId) => <TerminalPane
                  projectId={projectId} taskId={selectedTaskId()} taskHasWorktree={!!workspace()?.task.worktreePath}
                  transcript={workspace()?.transcript}
                  agentTerminals={workspace()?.task.id === selectedTaskId() && workspace()?.capabilities?.agentTerminals ? workspace()?.agentTerminals : undefined}
                  listEvent={terminalListEvent()} shown={inspectorTab() === 'terminals'}
                  onCommands={setTerminalCommands} taskTerminalShortcut={chordFor('newTaskTerminal')}
                  onShow={() => setInspectorTab('terminals')}
                  onLayoutLoaded={(layout) => restoreInspectorTab(projectId, layout.inspectorTab)}
                  attention={projectAttention().map((bell) => bell.terminalId)} onTerminalFocus={focusTerminal}
                />}
              </Show>
            </InspectorPanel>
            <InspectorPanel id="preview" active={inspectorTab()}>
              <Show when={selectedProjectId()} keyed fallback={<p class="empty">Select a project to preview.</p>}>
                {(projectId) => <PreviewPane projectId={projectId} url={selectedProject()?.previewUrl}
                  shown={inspectorTab() === 'preview'} canAddToPrompt={canAddPreviewNote()}
                  attachmentLimit={attachmentLimit()}
                  onAddToPrompt={addPreviewNote} onProjectSettings={() => openProjectSettings(projectId)} />}
              </Show>
            </InspectorPanel>
            <InspectorPanel id="review" active={inspectorTab()} lazy>
              <Show when={workspace()?.task.id} keyed fallback={<p class="empty">Select a task to review.</p>}>
                {(taskId) => <ReviewPane
                  task={workspace()!.task}
                  queuedNote={workspace()?.queuedReviewNote}
                  openFile={openTaskFile}
                  onAction={async (action) => {
                    const before = workspace()
                    const updated = await bridge.reviewTask(taskId, action)
                    if (selectedTaskId() === taskId) setWorkspace((current) => current === before ? updated : current)
                  }}
                />}
              </Show>
            </InspectorPanel>
            <InspectorPanel id="git" active={inspectorTab()} lazy>
              <Show when={workspace()?.task.worktreePath} fallback={<p class="empty">Select a task with a worktree.</p>}>
                <Show when={workspace()?.task.id} keyed>
                  {(taskId) => <GitPane openTerminal={() => setInspectorTab('terminals')} openFile={openTaskFile} taskId={taskId}
                    revision={workspace()!.task.updatedAt} live={LIVE_STATES.includes(workspace()!.task.state) || workspace()!.task.state === 'done'}
                    request={gitRequest()} onRequestHandled={(request) => setGitRequest((current) => current === request ? undefined : current)}
                    commitShortcut={chordFor('commit')} />}
                </Show>
              </Show>
            </InspectorPanel>
            <InspectorPanel id="files" active={inspectorTab()} lazy>
              <Show when={selectedProject()?.missing === false && selectedProjectId()} keyed
                fallback={<p class="empty">Select an available project to browse its files.</p>}>
                {(projectId) => <FilesPane projectId={projectId} initialFile={fileRequest()}
                  worktreeTaskId={workspace()?.task.worktreePath && workspace()?.task.id === selectedTaskId() ? selectedTaskId() : null}
                  showDotfiles={preferences()?.app['files.show_dotfiles'] ?? preferences()?.defaults['files.show_dotfiles'] ?? true}
                  settingsDisabled={!preferencesEditable() || savingPreference()}
                  onShowDotfilesChange={(value) => void requestPreference('files.show_dotfiles', value)} />}
              </Show>
            </InspectorPanel>
          </div>
        </aside>
      </WorkspaceFrame>

      <Show when={palette()} keyed>
        {(kind) => <CommandPalette title={kind === 'commands' ? 'Command palette' : kind === 'projects' ? 'Switch project' : 'Switch task'}
          entries={paletteEntries()} onClose={closePalette} onSelect={choosePaletteEntry} />}
      </Show>
      <Show when={archiveConfirmation()}>
        {(selection) => <ReviewArchiveDialog count={selection().length}
          onCancel={dismissArchiveConfirmation} onConfirm={archiveSelectedTasks} />}
      </Show>
      <Show when={deleteRequest()} keyed>
        {(task) => <DeleteTaskDialog title={task.title} worktreePath={task.worktreePath} pending={pending()}
          onCancel={() => setDeleteRequest(undefined)} onConfirm={deleteTask} />}
      </Show>

      <Show when={pickerOpen()}>
        <ProjectPicker
          scan={scan()}
          loading={scanning()}
          error={scanError()}
          onRescan={() => void scanProjects(true)}
          onCancel={() => setPickerOpen(false)}
          onSelect={addProject}
        />
      </Show>
    </>
  )
}

const INSPECTOR_MIN_WIDTH = 240
const INSPECTOR_MAX_WIDTH = 1600
const CHAT_MIN_WIDTH = 220

function InspectorPanel(props: { id: bridge.InspectorTab; active: bridge.InspectorTab; lazy?: boolean; children: JSX.Element }) {
  const shown = () => props.active === props.id
  return (
    <div class="inspector__panel" role="tabpanel" id={`inspector-panel-${props.id}`}
      aria-labelledby={`inspector-tab-${props.id}`} hidden={!shown()} tabIndex={0}>
      <Show when={!props.lazy || shown()}>{props.children}</Show>
    </div>
  )
}

function WorkspaceFrame(props: {
  children: JSX.Element
  footer: JSX.Element
  taskId: string | null
  inspectorWidth: number | null
  onInspectorWidthChange: (width: number) => Promise<void>
  search: JSX.Element
  sidebarHidden: boolean
  onCommands: () => void
  paletteShortcut?: string
  onBackToChat?: () => void
}) {
  const macWindow = isTauri() && navigator.platform.startsWith('Mac')
  const [metrics, setMetrics] = createSignal({ width: 360, limit: INSPECTOR_MAX_WIDTH })
  const [preview, setPreview] = createSignal<{ taskId: string | null; width: number }>()
  const [saving, setSaving] = createSignal(false)
  let body: HTMLDivElement | undefined
  let drag: { pointerId: number; taskId: string | null; x: number; width: number } | undefined
  const requestedWidth = () => {
    const current = preview()
    return current && current.taskId === props.taskId ? current.width : props.inspectorWidth
  }
  const clampWidth = (width: number) => Math.max(INSPECTOR_MIN_WIDTH, Math.min(metrics().limit, Math.round(width)))

  onMount(() => {
    const host = body
    const sidebar = host?.querySelector('.sidebar')
    const inspector = host?.querySelector('.inspector')
    if (!host || !sidebar || !inspector) return
    const measure = () => setMetrics({
      width: Math.round(inspector.getBoundingClientRect().width),
      limit: Math.max(INSPECTOR_MIN_WIDTH, Math.min(INSPECTOR_MAX_WIDTH,
        Math.floor(host.clientWidth - sidebar.getBoundingClientRect().width - CHAT_MIN_WIDTH))),
    })
    const observer = new ResizeObserver(measure)
    observer.observe(host)
    observer.observe(inspector)
    measure()
    onCleanup(() => observer.disconnect())
  })

  async function saveWidth(width: number) {
    setPreview({ taskId: props.taskId, width })
    setSaving(true)
    try {
      await props.onInspectorWidthChange(width)
    } finally {
      setPreview(undefined)
      setSaving(false)
    }
  }

  const dragRegion = macWindow ? '' : undefined

  return (
    <div class="shell" classList={{ 'shell--sidebar-hidden': props.sidebarHidden }} style={{
      '--workspace-inspector-width': requestedWidth() === null ? undefined : `${requestedWidth()}px`,
      '--inspector-min-width': `${INSPECTOR_MIN_WIDTH}px`,
      '--inspector-max-width': `${INSPECTOR_MAX_WIDTH}px`,
      '--chat-min-width': `${CHAT_MIN_WIDTH}px`,
    }}>
      <header class="shell__header" aria-label="Workspace header">
        <div class="shell__header-navigation" data-tauri-drag-region={dragRegion}>
          <Show when={macWindow}>
            <div class="shell__window-controls" data-tauri-drag-region="" aria-hidden="true" />
          </Show>
          <div class="shell__history" role="group" aria-label="Navigation history" data-tauri-drag-region={dragRegion}>
            <button class="shell__icon-button" type="button"
              aria-label={props.onBackToChat ? 'Back to chat' : 'Previous'}
              title={props.onBackToChat ? 'Back to chat' : 'Previous'}
              disabled={!props.onBackToChat} onClick={() => props.onBackToChat?.()}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="m14 6-6 6 6 6" />
              </svg>
            </button>
            <button class="shell__icon-button" type="button" aria-label="Next" title="Next" disabled>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="m10 6 6 6-6 6" />
              </svg>
            </button>
          </div>
          {props.search}
        </div>
      </header>
      <nav class="shell__rail" aria-label="Quick navigation">
        <button class="shell__icon-button" type="button" aria-label="Workspace" title="Workspace" disabled>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" aria-hidden="true">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="M9 4v16" />
          </svg>
        </button>
        <button class="shell__icon-button" type="button" aria-label="Conversations" title="Conversations" disabled>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" aria-hidden="true">
            <path d="M6 4h12a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-8l-5 3v-3a3 3 0 0 1-2-3V7a3 3 0 0 1 3-3Z" />
          </svg>
        </button>
        <button class="shell__icon-button" type="button" aria-label="Command palette" title="Command palette"
          aria-keyshortcuts={props.paletteShortcut} onClick={props.onCommands}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="3" y="4" width="18" height="16" rx="2" />
            <path d="m7 9 3 3-3 3m6 0h4" />
          </svg>
        </button>
      </nav>
      <div class="shell__body" ref={body}>
        {props.children}
        <div class="workspace-divider" role="separator" aria-label="Resize inspector"
          aria-controls="workspace-inspector" aria-orientation="vertical" tabIndex={0}
          aria-valuemin={INSPECTOR_MIN_WIDTH} aria-valuemax={metrics().limit}
          aria-valuenow={Math.max(INSPECTOR_MIN_WIDTH, metrics().width)}
          onPointerDown={(event) => {
            if (event.button !== 0 || saving() || drag) return
            event.preventDefault()
            event.currentTarget.focus({ preventScroll: true })
            event.currentTarget.setPointerCapture(event.pointerId)
            drag = { pointerId: event.pointerId, taskId: props.taskId, x: event.clientX, width: metrics().width }
          }}
          onPointerMove={(event) => {
            if (drag?.pointerId !== event.pointerId || drag.taskId !== props.taskId) return
            setPreview({ taskId: drag.taskId, width: clampWidth(drag.width + drag.x - event.clientX) })
          }}
          onPointerUp={(event) => {
            if (drag?.pointerId !== event.pointerId) return
            const start = drag
            drag = undefined
            const width = clampWidth(start.width + start.x - event.clientX)
            if (start.taskId === props.taskId && width !== start.width) void saveWidth(width)
            else setPreview(undefined)
          }}
          onLostPointerCapture={() => {
            if (!drag) return
            drag = undefined
            setPreview(undefined)
          }}
          onKeyDown={(event) => {
            if (saving() || drag) return
            const step = event.key === 'ArrowLeft' ? 32 : event.key === 'ArrowRight' ? -32 : 0
            if (!step) return
            event.preventDefault()
            const width = clampWidth(metrics().width + step)
            if (width !== metrics().width) void saveWidth(width)
          }} />
      </div>
      {props.footer}
    </div>
  )
}

interface PickerOption {
  id: string
  name: string
  detail?: string
  disabled?: boolean
}

interface PickerMenuProps {
  label: string
  text?: string
  value?: string
  options?: PickerOption[]
  groups?: Array<{ label: string; value: string; options: PickerOption[] }>
  onChange?: (value: string) => void
}

const DEFAULT_APPROVAL = { id: 'ask', name: 'Ask me' }
const APPROVAL_OPTIONS: PickerOption[] = [
  DEFAULT_APPROVAL,
  { id: 'automatic', name: 'Approve for me (coming soon)', disabled: true },
]

function ComposerPickers(props: { agent: bridge.Agent; agentName: string; agents: PickerOption[]; onAgentChange: (value: string) => void; showShellApprovals: boolean; canAttach: boolean; onAttach: () => void }) {
  return (
    <>
      <PickerMenu label="Attachments and commands" options={[
        { id: 'attach', name: 'Attach files', disabled: !props.canAttach },
        { id: 'commands', name: 'Commands (coming soon)', disabled: true },
      ]} onChange={(value) => { if (value === 'attach') props.onAttach() }} triggerClass="composer__picker" above>
        <PlusIcon />
      </PickerMenu>
      <Show when={props.showShellApprovals}>
        <div class="composer__approval-slot">
          <PickerMenu label="Shell approvals" text={DEFAULT_APPROVAL.name} value={DEFAULT_APPROVAL.id} options={APPROVAL_OPTIONS} triggerClass="composer__picker" above>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M12 21s7-3.2 7-9V5l-7-3-7 3v7c0 5.8 7 9 7 9Z" />
              <path d="m8.5 9 2.5 2.5L8.5 14m4.5 0h2.5" />
            </svg>
            <span class="composer__picker-label">{DEFAULT_APPROVAL.name}</span>
            <PickerCaret />
          </PickerMenu>
        </div>
      </Show>
      <PickerMenu label="Agent" text={props.agentName} value={props.agent} options={props.agents} onChange={props.onAgentChange} triggerClass="composer__picker" above>
        <span class="composer__picker-label">{props.agentName}</span>
        <PickerCaret />
      </PickerMenu>
    </>
  )
}

function ComposerModePicker(props: { modes: bridge.AgentModes; disabled: boolean; defaultOption?: string; onChange: (mode: string) => void }) {
  const label = () => props.modes.options.find((option) => option.id === props.modes.current)?.name
    ?? props.modes.current ?? 'Default mode'
  const options = () => [
    ...(props.defaultOption ? [{ id: '', name: props.defaultOption, disabled: props.disabled }] : []),
    ...props.modes.options.map((option) => ({
      id: option.id, name: option.name, detail: option.description ?? undefined, disabled: props.disabled,
    })),
  ]
  return (
    <PickerMenu label="Mode" text={label()} value={props.modes.current ?? ''} options={options()}
      onChange={props.onChange} triggerClass="composer__picker" above>
      <span class="composer__picker-label">{label()}</span>
      <PickerCaret />
    </PickerMenu>
  )
}

function ComposerModelPicker(props: {
  model: string | null
  effort?: string | null
  models: bridge.CodexModel[]
  options?: bridge.CodexTurnOptions
  disabled: boolean
  onChange: (options: bridge.CodexTurnOptions) => void
}) {
  const modelLabel = () => props.model ?? 'Codex default'
  const modelInfo = () => props.models.find((model) => model.model === props.model)
  const modelOptions = () => [
    ...(!modelInfo() ? [{ id: `model:${modelLabel()}`, name: modelLabel(), disabled: true }] : []),
    ...props.models.map((model) => ({
      id: `model:${model.model}`, name: model.model, detail: model.displayName, disabled: props.disabled,
    })),
  ]
  const effortOptions = () => modelInfo()?.supportedReasoningEfforts.map(({ reasoningEffort, description }) => ({
    id: `effort:${reasoningEffort}`,
    name: reasoningEffort.charAt(0).toUpperCase() + reasoningEffort.slice(1),
    detail: description,
    disabled: props.disabled,
  })) ?? []
  const groups = () => {
    const efforts = effortOptions()
    return [
      { label: 'Model', value: `model:${modelLabel()}`, options: modelOptions() },
      {
        label: 'Reasoning effort',
        value: `effort:${props.effort ?? modelInfo()?.defaultReasoningEffort ?? ''}`,
        options: efforts.length ? efforts : [{ id: 'unavailable', name: 'Effort unavailable', disabled: true }],
      },
    ]
  }
  function choose(id: string) {
    if (id.startsWith('model:')) {
      const model = props.models.find((item) => item.model === id.slice(6))
      if (model) props.onChange({ model: model.model, effort: model.defaultReasoningEffort ?? undefined })
    } else if (id.startsWith('effort:')) {
      props.onChange({ ...props.options, effort: id.slice(7) })
    }
  }
  return (
    <PickerMenu label="Model / effort" text={modelLabel()} groups={groups()} onChange={choose} triggerClass="composer__picker" above>
      <span class="composer__picker-label">{modelLabel()}</span>
      <PickerCaret />
    </PickerMenu>
  )
}

function HeadlinePicker(props: PickerMenuProps & { text: string }) {
  return (
    <PickerMenu {...props} triggerClass="headline-picker">
      <span class="headline-picker__text">{props.text}</span>
      <PickerCaret />
    </PickerMenu>
  )
}

function PickerCaret() {
  return (
    <svg class="picker-caret" width="0.45em" height="0.45em" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="m2.5 4.5 3.5 3.5 3.5-3.5" />
    </svg>
  )
}

function PickerMenu(props: PickerMenuProps & { triggerClass: string; above?: boolean; children: JSX.Element }) {
  const menuId = createUniqueId()
  const [open, setOpen] = createSignal(false)
  const triggerLabel = () => props.text ? `${props.label}: ${props.text}` : props.label
  let trigger!: HTMLButtonElement
  let menu!: HTMLSpanElement
  const groups = () => props.groups ?? [{ label: undefined, value: props.value, options: props.options ?? [] }]

  function positionMenu() {
    const bounds = trigger.getBoundingClientRect()
    const viewportInset = 12
    const gap = 6
    const below = window.innerHeight - bounds.bottom - gap - viewportInset
    const above = bounds.top - gap - viewportInset
    menu.style.removeProperty('--picker-menu-room')
    const opensAbove = props.above ? menu.offsetHeight <= above || above >= below : menu.offsetHeight > below && above > below
    menu.style.setProperty('--picker-menu-room', `${Math.max(0, opensAbove ? above : below)}px`)
    const left = Math.min(bounds.left, window.innerWidth - menu.offsetWidth - viewportInset)
    menu.style.left = `${Math.max(viewportInset, left)}px`
    menu.style.top = `${opensAbove ? bounds.top - gap - menu.offsetHeight : bounds.bottom + gap}px`
  }

  function closeMenu() {
    menu.hidePopover()
    trigger.focus({ preventScroll: true })
  }

  createEffect(() => {
    if (!open()) return
    const transcript = trigger.closest('.chat__transcript')
    const dismiss = () => menu.hidePopover()
    transcript?.addEventListener('scroll', dismiss)
    window.addEventListener('resize', positionMenu)
    onCleanup(() => {
      transcript?.removeEventListener('scroll', dismiss)
      window.removeEventListener('resize', positionMenu)
    })
  })

  return (
    <>
      <button
        ref={trigger}
        class={props.triggerClass}
        type="button"
        aria-label={triggerLabel()}
        title={props.above ? triggerLabel() : undefined}
        aria-haspopup="menu"
        aria-controls={menuId}
        aria-expanded={open()}
        popovertarget={menuId}
      >
        {props.children}
      </button>
      <span
        ref={menu}
        id={menuId}
        class="picker-menu"
        popover="auto"
        role="menu"
        tabindex={-1}
        aria-label={props.label}
        on:toggle={(event) => {
          const isOpen = event.newState === 'open'
          setOpen(isOpen)
          if (!isOpen) return
          positionMenu()
          const current = menu.querySelector<HTMLButtonElement>('button[aria-checked="true"]:enabled')
            ?? menu.querySelector<HTMLButtonElement>('button:enabled')
            ?? menu
          current.focus()
        }}
        onKeyDown={(event) => {
          if (event.key === 'Tab') {
            closeMenu()
            return
          }
          if (event.key === 'Escape') {
            event.preventDefault()
            closeMenu()
            return
          }
          if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
          event.preventDefault()
          const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('button:enabled'))
          if (!items.length) return
          const current = items.findIndex((item) => item === document.activeElement)
          const step = event.key === 'ArrowUp' ? -1 : 1
          let next = (current + step + items.length) % items.length
          if (event.key === 'Home') next = 0
          else if (event.key === 'End') next = items.length - 1
          items[next]?.focus()
        }}
      >
        <For each={groups()}>{(group) =>
          <span class="picker-menu__group" role={group.label ? 'group' : undefined} aria-label={group.label}>
            <Show when={group.label}><span class="picker-menu__label" aria-hidden="true">{group.label}</span></Show>
            <For each={group.options}>{(option) => (
              <button
                type="button"
                class="nav-item"
                role={group.value === undefined ? 'menuitem' : 'menuitemradio'}
                tabindex={-1}
                aria-checked={group.value === undefined ? undefined : group.value === option.id}
                title={option.detail}
                disabled={option.disabled}
                onClick={() => {
                  closeMenu()
                  if (option.id !== group.value) props.onChange?.(option.id)
                }}
              >
                <span>{option.name}</span>
                <Show when={group.value === option.id}>
                  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="m3.5 8 3 3 6-6" />
                  </svg>
                </Show>
              </button>
            )}</For>
          </span>
        }</For>
      </span>
    </>
  )
}

interface InspectorTabOption {
  id: bridge.InspectorTab
  label: string
  enabled: boolean
  icon: string
}

function FolderPlusIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M12 10v6" />
      <path d="M9 13h6" />
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </svg>
  )
}

function PlusIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  )
}

const INSPECTOR_TABS: InspectorTabOption[] = [
  { id: 'terminals', label: 'Terminals', enabled: true, icon: 'M3 6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Zm4 3 3 3-3 3m6 0h4' },
  { id: 'preview', label: 'Preview', enabled: true, icon: 'M2 12s3.6-6 10-6 10 6 10 6-3.6 6-10 6S2 12 2 12Zm13 0a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z' },
  { id: 'review', label: 'Review', enabled: true, icon: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Zm0 0v6h6M8 14l3 3 5-5' },
  { id: 'git', label: 'Git', enabled: true, icon: 'M6 6a2 2 0 1 0 0-4 2 2 0 0 0 0 4Zm0 16a2 2 0 1 0 0-4 2 2 0 0 0 0 4ZM6 6v12M18 9a2 2 0 1 0 0-4 2 2 0 0 0 0 4Zm0 0c0 4-4 6-12 6' },
  { id: 'files', label: 'Files', enabled: true, icon: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z' },
]

function Dot() {
  return (
    <span class="status-bar__dot" aria-hidden="true">
      ·
    </span>
  )
}

function agentActivity(workspace: bridge.TaskWorkspace | null): string {
  switch (workspace?.task.state) {
    case 'working':
      return 'agent working'
    case 'waiting':
      return 'waiting for you'
    case 'starting':
      return 'agent starting'
    case 'queued':
      return 'agent queued'
    case 'ready':
      return 'agent idle'
    case 'review':
      return 'ready for review'
    case 'failed':
      return 'agent failed'
    case 'interrupted':
      return 'task interrupted'
    case 'cancelled':
      return 'task cancelled'
    case 'done':
      return 'task archived'
    case 'draft':
      return 'draft'
    default:
      return 'no agent'
  }
}

function describe(failure: unknown): string {
  if (typeof failure === 'object' && failure !== null && 'message' in failure) {
    return String(failure.message)
  }
  return String(failure)
}

function formatArguments(argumentsValue: unknown): string {
  if (argumentsValue === null || argumentsValue === undefined) return ''
  if (typeof argumentsValue === 'string') return argumentsValue
  return JSON.stringify(argumentsValue, null, 2)
}

function DiffBlock(props: { patch: bridge.FileDiff }) {
  return (
    <div class="entry__diff">
      <p class="entry__diff-path">{props.patch.path}</p>
      <Show when={props.patch.oldText}>
        {(oldText) => (
          <For each={oldText().split('\n')}>
            {(line) => <p class="entry__diff-removed">{`- ${line}`}</p>}
          </For>
        )}
      </Show>
      <For each={props.patch.newText.split('\n')}>
        {(line) => <p class="entry__diff-added">{`+ ${line}`}</p>}
      </For>
    </div>
  )
}

function SearchMark(props: { text: string; match?: string }) {
  const index = () => props.match ? props.text.indexOf(props.match) : -1
  return <Show when={index() >= 0} fallback={props.text}>
    {props.text.slice(0, index())}<mark class="search-mark">{props.match}</mark>{props.text.slice(index() + (props.match?.length ?? 0))}
  </Show>
}

function entryBody(
  entry: bridge.Entry,
  decide: (approvalId: string, decision: bridge.Decision) => void,
  cancelling: () => boolean,
  canApprove: () => boolean,
  match: () => string | undefined,
) {
  const kind = entry.kind
  const text = (value: string) => <SearchMark text={value} match={match()} />
  const failureLabels: Record<string, string> = {
    missingBinary: 'Agent not installed',
    unsupportedVersion: 'Version not supported',
    authRequired: 'Sign-in required',
    agentRejected: 'Agent rejected the turn',
    protocol: 'Protocol error',
    midTurnExit: 'Agent exited mid-turn',
  }
  switch (kind.type) {
    case 'userMessage':
      return <p class="entry__user">{text(kind.data.text)}</p>
    case 'agentText':
      return <p class="entry__agent">{text(kind.data.text)}</p>
    case 'thought':
      return <p class="entry__thought">{text(kind.data.text)}</p>
    case 'toolCall':
      return (
        <div class="entry__tool">
          <span class="entry__tool-title">{text(kind.data.title)}</span>
          <span class="entry__tool-status">{kind.data.status}</span>
          <Show when={kind.data.output}>
            {(output) => <pre class="entry__output">{text(output())}</pre>}
          </Show>
        </div>
      )
    case 'plan':
      return (
        <ul class="entry__plan">
          <For each={kind.data.rows}>
            {(row) => (
              <li>
                <span class="entry__plan-status">{row.status}</span> {text(row.text)}
              </li>
            )}
          </For>
        </ul>
      )
    case 'permission': {
      const permission = kind.data
      const argumentsText = formatArguments(permission.arguments)
      const decisionText = permission.withdrawn ? 'withdrawn'
        : permission.source === 'sessionGrant' ? 'allowed by session grant'
        : permission.source === 'projectGrant' ? 'allowed by project grant'
        : permission.decision === 'allowSession' ? 'allowed for session'
        : permission.decision === 'allowProject' ? 'allowed for project'
        : permission.decision
      return (
        <div class="entry__permission">
          <p class="entry__permission-title">{text(permission.title)}</p>
          <Show when={permission.paths.length > 0}>
            <p class="entry__permission-paths">{permission.paths.join(', ')}</p>
          </Show>
          <Show when={argumentsText.length > 0}>
            <pre class="entry__arguments">{argumentsText}</pre>
          </Show>
          <Show
            when={permission.patch}
            fallback={
              <p class="entry__permission-note">
                diff not available; the arguments above are the whole request
              </p>
            }
          >
            {(patch) => <DiffBlock patch={patch()} />}
          </Show>
          <Show
            when={canApprove() && !permission.withdrawn && permission.decision === null}
            fallback={<span class="entry__permission-decision">{decisionText ?? 'approval unavailable'}</span>}
          >
            <div class="entry__actions">
              <For each={permission.choices ?? DEFAULT_APPROVAL_CHOICES}>
                {(choice) => (
                  <button
                    type="button"
                    data-decision={choice}
                    disabled={cancelling()}
                    onClick={() => decide(permission.approvalId, choice)}
                  >
                    {choice === 'allow' && !permission.choices ? 'Allow' : APPROVAL_LABELS[choice]}
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>
      )
    }
    case 'notice':
      return <p class={kind.data.isError ? 'entry__notice--error' : 'entry__notice'}>{text(kind.data.text)}</p>
    case 'failure':
      return (
        <div class="entry__failure">
          <p class="entry__failure-title">
            {failureLabels[kind.data.kind] ?? kind.data.kind}
          </p>
          <p class="entry__failure-text">{text(kind.data.text)}</p>
          <Show when={kind.data.detail}>
            {(detail) => <pre class="entry__failure-detail">{detail()}</pre>}
          </Show>
        </div>
      )
    case 'usage':
      return <p class="entry__usage">{`${kind.data.used} of ${kind.data.size} tokens`}</p>
  }
}
