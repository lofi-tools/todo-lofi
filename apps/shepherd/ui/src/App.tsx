/**
 * The fixed task-workspace shell. Regions never move between each other; every
 * value comes from the core's snapshots through the bridge, never from a second
 * frontend model.
 */

import { For, Show, createSignal, onCleanup, onMount } from 'solid-js'
import { isTauri } from '@tauri-apps/api/core'
import { homeDir } from '@tauri-apps/api/path'
import { getCurrentWindow } from '@tauri-apps/api/window'

import * as bridge from './bridge'
import { ProjectImportSources, ProjectPicker } from './ProjectPicker'
import { ProjectRow } from './ProjectRow'
import { GitPane } from './GitPane'
import { ReviewPane } from './ReviewPane'
import { SettingsPane } from './SettingsPane'
import { TerminalPane } from './TerminalPane'

type CoreEvent = bridge.Event
type TerminalBell = Extract<CoreEvent, { type: 'terminalBell' }>['data']

const LIVE_STATES: bridge.TaskState[] = ['starting', 'queued', 'working', 'waiting']

export function App() {
  const [projects, setProjects] = createSignal<bridge.ProjectSnapshot[]>([])
  const [homeDirectory, setHomeDirectory] = createSignal<string>()
  const [projectsLoaded, setProjectsLoaded] = createSignal(false)
  const [codexAvailable, setCodexAvailable] = createSignal(false)
  const [selectedProjectId, setSelectedProjectId] = createSignal<string | null>(null)
  const [tasks, setTasks] = createSignal<bridge.TaskSummary[]>([])
  const [selectedTaskId, setSelectedTaskId] = createSignal<string | null>(null)
  const [workspace, setWorkspace] = createSignal<bridge.TaskWorkspace | null>(null)
  const [showArchived, setShowArchived] = createSignal(false)
  const [archiveVisibility, setArchiveVisibility] = createSignal(new Map<string, boolean>())
  const [projectMenuId, setProjectMenuId] = createSignal<string>()
  const [settingsOpen, setSettingsOpen] = createSignal(false)
  const [projectSettingsRequest, setProjectSettingsRequest] = createSignal<{ id: string }>()
  const showsArchived = (projectId: string | null) =>
    (projectId ? archiveVisibility().get(projectId) : undefined) ?? showArchived()
  const settingsProject = () => projects().find((project) => project.id === projectSettingsRequest()?.id)
  const visibleTasks = () => tasks().filter((task) => showsArchived(selectedProjectId()) || task.state !== 'done')

  function setProjectArchived(projectId: string, visible: boolean) {
    setArchiveVisibility((previous) => new Map(previous).set(projectId, visible))
  }
  const [terminalListEvent, setTerminalListEvent] = createSignal<Extract<CoreEvent, { type: 'terminalListChanged' }>['data']>()
  const [inspectorTab, setInspectorTab] = createSignal('terminals')
  const [terminalAttention, setTerminalAttention] = createSignal<TerminalBell[]>([])
  const [focusedTerminal, setFocusedTerminal] = createSignal<string | null>(null)
  const projectAttention = () => terminalAttention().filter((bell) => bell.projectId === selectedProjectId())
  const tabAttention = () => projectAttention().some((bell) => bell.taskId === null || bell.taskId === selectedTaskId())

  function focusTerminal(id: string | null) {
    setFocusedTerminal(id)
    if (id) setTerminalAttention((bells) => bells.filter((bell) => bell.terminalId !== id))
  }
  const [prompt, setPrompt] = createSignal('')
  const [pickerOpen, setPickerOpen] = createSignal(false)
  const [scan, setScan] = createSignal<bridge.ProjectScan | null>(null)
  const [scanning, setScanning] = createSignal(false)
  const [scanError, setScanError] = createSignal<string | null>(null)
  const [importNote, setImportNote] = createSignal<string | null>(null)
  const [error, setError] = createSignal<string | null>(null)
  const [pending, setPending] = createSignal(false)
  const [quitRequired, setQuitRequired] = createSignal<number | null>(null)
  const [shutdownNote, setShutdownNote] = createSignal<string | null>(null)
  let allowClose = false

  const runningCount = () => tasks().filter((task) => LIVE_STATES.includes(task.state)).length
  const reviewCount = () => projects().reduce((count, project) => count + (project.reviewTaskCount ?? 0), 0)
  const canCompose = () => {
    const state = workspace()?.task.state
    return state === 'draft' || state === 'ready'
  }

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
    if (selectedProjectId() === projectId) setTasks(list.tasks)
    return list.tasks
  }

  async function selectTask(taskId: string) {
    setSelectedTaskId(taskId)
    setWorkspace(null)
    const next = await bridge.taskWorkspace(taskId)
    if (selectedTaskId() === taskId) setWorkspace(next)
  }

  async function selectProject(project: bridge.ProjectSnapshot) {
    setTerminalListEvent(undefined)
    setSelectedProjectId(project.id)
    setSelectedTaskId(null)
    setWorkspace(null)
    setTasks([])
    const listed = await loadTasks(project.id)
    if (selectedProjectId() !== project.id) return
    const lastTask = project.lastTaskId
      ? listed.find((task) => task.id === project.lastTaskId)
      : undefined
    if (lastTask && (showsArchived(project.id) || lastTask.state !== 'done')) await selectTask(lastTask.id)
  }

  function handleEvent(event: CoreEvent) {
    switch (event.type) {
      case 'projectListChanged':
        setProjects(event.data.projects)
        break
      case 'taskListChanged':
        if (event.data.projectId === selectedProjectId()) setTasks(event.data.tasks)
        break
      case 'taskWorkspaceChanged':
        if (event.data.task.id === selectedTaskId()) setWorkspace(event.data)
        break
      case 'taskChanged': {
        const projectId = selectedProjectId()
        if (projectId) {
          void run(async () => {
            await loadTasks(projectId)
          })
        }
        break
      }
      case 'terminalBell':
        if (event.data.terminalId === focusedTerminal() && inspectorTab() === 'terminals' && document.hasFocus()) break
        setTerminalAttention((bells) => bells.some((bell) => bell.terminalId === event.data.terminalId) ? bells : [...bells, event.data])
        break
      case 'terminalListChanged':
        if (event.data.projectId === selectedProjectId()) setTerminalListEvent(event.data)
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
    void homeDir().then(setHomeDirectory).catch((error) => console.warn('Could not resolve the home directory', error))
    void run(async () => {
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
    setSettingsOpen(false)
    void run(async () => {
      const draft = await bridge.createDraft(projectId, 'New task')
      if (selectedProjectId() !== projectId) {
        setTerminalListEvent(undefined)
        setSelectedProjectId(projectId)
        setTasks([])
      }
      setSelectedTaskId(draft.task.id)
      setWorkspace(draft)
      await loadTasks(projectId)
    })
  }

  function discardDraft() {
    const current = workspace()
    if (!current) return
    void run(async () => {
      await bridge.discardDraft(current.task.id)
      setWorkspace(null)
      setSelectedTaskId(null)
      const projectId = selectedProjectId()
      if (projectId) await loadTasks(projectId)
    })
  }

  function sendPrompt() {
    const current = workspace()
    const text = prompt().trim()
    if (!current || !text || !canCompose()) return
    setPrompt('')
    void run(async () => {
      setWorkspace(await bridge.sendPrompt(current.task.id, text))
      const projectId = selectedProjectId()
      if (projectId) await loadTasks(projectId)
    })
  }

  function decide(approvalId: string, decision: bridge.Decision) {
    const current = workspace()
    if (!current) return
    void run(async () => {
      setWorkspace(await bridge.resolveApproval(current.task.id, approvalId, decision))
      const projectId = selectedProjectId()
      if (projectId) await loadTasks(projectId)
    })
  }

  function quit() {
    void run(async () => {
      try {
        const summary = await bridge.quit(false)
        setShutdownNote(`quit: ${summary.interruptedTasks} interrupted`)
        allowClose = true
        await getCurrentWindow().close()
      } catch (failure) {
        const message = describe(failure)
        if (message.includes('requires confirmation')) {
          setQuitRequired(Number(message.match(/(\d+) live task/)?.[1] ?? 1))
        } else {
          setError(message)
        }
      }
    })
  }

  function forceQuit() {
    void run(async () => {
      const summary = await bridge.quit(true)
      setQuitRequired(null)
      setShutdownNote(`quit: ${summary.interruptedTasks} interrupted`)
      allowClose = true
      await getCurrentWindow().close()
    })
  }

  return (
    <div class="shell">
      <Show when={isTauri() && navigator.platform.startsWith('Mac')}>
        <div class="shell__window-drag" data-tauri-drag-region="" aria-hidden="true" />
      </Show>
      <div class="shell__body">
        <aside class="sidebar" aria-label="Workspace navigation">
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
              <PlusIcon />
            </button>
          </header>

          <ul class="projects">
            <Show when={projectsLoaded() && projects().length === 0}>
              <li class="projects__empty">
                <button type="button" class="project-action projects__new" onClick={openPicker} disabled={pending()}>
                  <PlusIcon />
                  New project
                </button>
                <p class="empty">or import all from</p>
                <ProjectImportSources codexDisabled={pending() || !codexAvailable()} note={importNote()} onImportCodex={importProjects} />
              </li>
            </Show>
            <For each={projects()}>
              {(project) => (
                <li>
                  <ProjectRow
                    project={project}
                    homeDirectory={homeDirectory()}
                    selected={project.id === selectedProjectId()}
                    pending={pending()}
                    showArchived={showsArchived(project.id)}
                    menuOpen={projectMenuId() === project.id}
                    onSelect={() => {
                      setSettingsOpen(false)
                      void run(() => selectProject(project))
                    }}
                    onCompose={() => newTask(project.id)}
                    onMenuOpenChange={(open) => {
                      if (open) setProjectMenuId(project.id)
                      else if (projectMenuId() === project.id) setProjectMenuId(undefined)
                    }}
                    onToggleArchived={() => setProjectArchived(project.id, !showsArchived(project.id))}
                    onProjectSettings={() => {
                      setProjectSettingsRequest({ id: project.id })
                      setSettingsOpen(true)
                    }}
                  />
                  <Show when={project.missing}>
                    <p class="project__missing">path is missing</p>
                  </Show>

                  <Show when={project.id === selectedProjectId() && visibleTasks().length > 0}>
                    <div class="tasks">
                      <ul class="task-list">
                        <For each={visibleTasks()}>
                          {(task) => (
                            <li>
                              <button
                                type="button"
                                class={`task task--${task.state}`}
                                aria-current={task.id === selectedTaskId() ? 'true' : undefined}
                                onClick={() => {
                                  setSettingsOpen(false)
                                  void run(() => selectTask(task.id))
                                }}
                              >
                                <span class="task__state" aria-hidden="true" />
                                <span class="task__title">{task.title}</span>
                                <Show when={projectAttention().some((bell) => bell.taskId === task.id)}>
                                  <span class="terminal-attention" role="img" aria-label="Terminal needs attention" />
                                </Show>
                                <Show when={task.state === 'review'} fallback={<span class="task__state-label">{task.state}</span>}>
                                  <ReviewBadge />
                                </Show>
                              </button>
                            </li>
                          )}
                        </For>
                      </ul>
                    </div>
                  </Show>
                </li>
              )}
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
            showArchived={showArchived()}
            onShowArchived={setShowArchived}
            onAddProject={openPicker}
            pending={pending()}
            projectImports={<ProjectImportSources codexDisabled={pending() || !codexAvailable()} note={importNote()} onImportCodex={importProjects} />}
            project={settingsProject()}
            projectSettingsRequest={projectSettingsRequest()}
            showProjectArchived={showsArchived(projectSettingsRequest()?.id ?? null)}
            onShowProjectArchived={setProjectArchived}
          />
        </Show>

        <section class="chat" aria-label="Task chat" hidden={settingsOpen()}>
          <header class="pane-header">
            <span class="chat__title">{workspace()?.task.title ?? 'No task selected'}</span>
            <Show when={workspace()}>
              {(current) => <span class="chat__state">{current().task.state}</span>}
            </Show>
          </header>

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

          <Show when={workspace()?.diagnostics}>
            {(diagnostics) => (
              <details class="diagnostics">
                <summary>Diagnostics</summary>
                <dl>
                  <dt>Command</dt>
                  <dd>
                    <code>{diagnostics().command || '—'}</code>
                  </dd>
                  <dt>Protocol</dt>
                  <dd>{diagnostics().protocol || '—'}</dd>
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

          <div class="chat__transcript">
            <Show
              when={workspace()}
              fallback={<p class="empty">Select a task to read its transcript.</p>}
            >
              {(current) => (
                <Show
                  when={current().transcript.length > 0}
                  fallback={<p class="empty">Send a prompt to start this task.</p>}
                >
                  <ol class="transcript">
                    <For each={current().transcript}>
                      {(entry) => <li class="entry">{entryBody(entry, decide)}</li>}
                    </For>
                  </ol>
                </Show>
              )}
            </Show>
          </div>

          <div class="composer">
            <textarea
              class="composer__input"
              aria-label="Prompt"
              placeholder={canCompose() ? 'Send a prompt' : 'Select a draft or ready task'}
              rows={2}
              value={prompt()}
              disabled={!canCompose() || pending()}
              onInput={(event) => setPrompt(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && !event.shiftKey) {
                  event.preventDefault()
                  sendPrompt()
                }
              }}
            />
            <button
              type="button"
              class="composer__send"
              onClick={sendPrompt}
              disabled={!canCompose() || pending() || !prompt().trim()}
            >
              Send
            </button>
          </div>
        </section>

        <aside class="inspector" aria-label="Inspector">
          <div
            class="inspector__tabs"
            role="tablist"
            aria-orientation="vertical"
            aria-label="Inspector tabs"
          >
            <For each={INSPECTOR_TABS}>
              {(tab) => (
                <button
                  type="button"
                  role="tab"
                  class="inspector__tab"
                  id={`inspector-tab-${tab.id}`}
                  aria-selected={tab.id === inspectorTab()}
                  onClick={() => setInspectorTab(tab.id)}
                  aria-disabled={!tab.enabled}
                  aria-controls="inspector-pane"
                  disabled={!tab.enabled}
                  title={tab.enabled ? tab.label : `${tab.label} arrives in a later slice`}
                >
                  {tab.label}
                  <Show when={tab.id === 'terminals' && tabAttention()}>
                    <span class="terminal-attention" role="img" aria-label="Terminal needs attention" />
                  </Show>
                  <Show when={tab.id === 'review' && workspace()?.task.state === 'review'}>
                    <ReviewBadge />
                  </Show>
                </button>
              )}
            </For>
          </div>
          <div
            class="inspector__pane"
            id="inspector-pane"
            role="tabpanel"
            aria-labelledby={`inspector-tab-${inspectorTab()}`}
          >
            <div class="terminal-tab" hidden={inspectorTab() !== 'terminals'}>
              <Show when={selectedProjectId()} keyed fallback={<p class="empty">No terminals yet.</p>}>
                {(projectId) => <TerminalPane projectId={projectId} taskId={selectedTaskId()} taskHasWorktree={!!workspace()?.task.worktreePath} listEvent={terminalListEvent()} shown={inspectorTab() === 'terminals'} attention={projectAttention().map((bell) => bell.terminalId)} onTerminalFocus={focusTerminal} />}
              </Show>
            </div>
            <Show when={inspectorTab() === 'review'}>
              <Show when={workspace()?.task.id} keyed fallback={<p class="empty">Select a task to review.</p>}>
                {(taskId) => <ReviewPane
                  task={workspace()!.task}
                  queuedNote={workspace()?.queuedReviewNote}
                  onAction={async (action) => {
                    const updated = await bridge.reviewTask(taskId, action)
                    if (selectedTaskId() === taskId) setWorkspace(updated)
                  }}
                />}
              </Show>
            </Show>
            <Show when={inspectorTab() === 'git'}>
              <Show when={workspace()?.task.worktreePath} fallback={<p class="empty">Select a task with a worktree.</p>}>
                <Show when={workspace()?.task.id} keyed>
                  {(taskId) => <GitPane openTerminal={() => setInspectorTab('terminals')} taskId={taskId} revision={workspace()!.task.updatedAt} live={LIVE_STATES.includes(workspace()!.task.state) || workspace()!.task.state === 'done'} />}
                </Show>
              </Show>
            </Show>
          </div>
        </aside>
      </div>

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

      <Show when={quitRequired()}>
        <div class="banner" role="alert">
          <span>
            Quit will stop {quitRequired()} live task{quitRequired() === 1 ? '' : 's'}. Quit anyway?
          </span>
          <button type="button" onClick={forceQuit} disabled={pending()}>
            Quit anyway
          </button>
          <button type="button" class="banner__quiet" onClick={() => setQuitRequired(null)}>
            Keep working
          </button>
        </div>
      </Show>

      <Show when={error()}>
        {(message) => (
          <div class="banner banner--error" role="alert">
            <span>{message()}</span>
            <button type="button" class="banner__quiet" onClick={() => setError(null)}>
              Dismiss
            </button>
          </div>
        )}
      </Show>

      <footer class="status-bar" aria-label="Status">
        <span>{agentActivity(workspace())}</span>
        <Dot />
        <span>{runningCount()} tasks running</span>
        <Dot />
        <span>{reviewCount()} in review</span>
        <Dot />
        <span>branch {workspace()?.task.branch ?? '—'}</span>
        <Dot />
        <span>changes —</span>
        <Show when={shutdownNote()}>
          {(note) => (
            <>
              <Dot />
              <span>{note()}</span>
            </>
          )}
        </Show>
      </footer>
    </div>
  )
}

interface InspectorTab {
  id: string
  label: string
  enabled: boolean
}

function PlusIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  )
}

const INSPECTOR_TABS: InspectorTab[] = [
  { id: 'terminals', label: 'Terminals', enabled: true },
  { id: 'preview', label: 'Preview', enabled: false },
  { id: 'review', label: 'Review', enabled: true },
  { id: 'git', label: 'Git', enabled: true },
  { id: 'files', label: 'Files', enabled: false },
]

function ReviewBadge() {
  return <span class="review-badge">Needs review</span>
}

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

function entryBody(
  entry: bridge.Entry,
  decide: (approvalId: string, decision: bridge.Decision) => void,
) {
  const kind = entry.kind
  const failureLabels: Record<string, string> = {
    missingBinary: 'Agent not installed',
    unsupportedVersion: 'Version not supported',
    authRequired: 'Sign-in required',
    protocol: 'Protocol error',
    midTurnExit: 'Agent exited mid-turn',
  }
  switch (kind.type) {
    case 'userMessage':
      return <p class="entry__user">{kind.data.text}</p>
    case 'agentText':
      return <p class="entry__agent">{kind.data.text}</p>
    case 'thought':
      return <p class="entry__thought">{kind.data.text}</p>
    case 'toolCall':
      return (
        <div class="entry__tool">
          <span class="entry__tool-title">{kind.data.title}</span>
          <span class="entry__tool-status">{kind.data.status}</span>
          <Show when={kind.data.output}>
            {(output) => <pre class="entry__output">{output()}</pre>}
          </Show>
        </div>
      )
    case 'plan':
      return (
        <ul class="entry__plan">
          <For each={kind.data.rows}>
            {(row) => (
              <li>
                <span class="entry__plan-status">{row.status}</span> {row.text}
              </li>
            )}
          </For>
        </ul>
      )
    case 'permission': {
      const permission = kind.data
      const argumentsText = formatArguments(permission.arguments)
      return (
        <div class="entry__permission">
          <p class="entry__permission-title">{permission.title}</p>
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
            when={permission.decision === null}
            fallback={<span class="entry__permission-decision">{permission.decision}</span>}
          >
            <div class="entry__actions">
              <button type="button" onClick={() => decide(permission.approvalId, 'allow')}>
                Allow
              </button>
              <button type="button" onClick={() => decide(permission.approvalId, 'deny')}>
                Deny
              </button>
            </div>
          </Show>
        </div>
      )
    }
    case 'notice':
      return <p class={kind.data.isError ? 'entry__notice--error' : 'entry__notice'}>{kind.data.text}</p>
    case 'failure':
      return (
        <div class="entry__failure">
          <p class="entry__failure-title">
            {failureLabels[kind.data.kind] ?? kind.data.kind}
          </p>
          <p class="entry__failure-text">{kind.data.text}</p>
          <Show when={kind.data.detail}>
            {(detail) => <pre class="entry__failure-detail">{detail()}</pre>}
          </Show>
        </div>
      )
    case 'usage':
      return <p class="entry__usage">{`${kind.data.used} of ${kind.data.size} tokens`}</p>
  }
}
