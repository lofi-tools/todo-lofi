/**
 * The fixed task-workspace shell. Regions never move between each other; every
 * value comes from the core's snapshots through the bridge, never from a second
 * frontend model.
 */

import { For, Show, createSignal, onMount } from 'solid-js'
import { getCurrentWindow } from '@tauri-apps/api/window'

import * as bridge from './bridge'
import { ProjectPicker } from './ProjectPicker'
import { GitPane } from './GitPane'
import { ReviewPane } from './ReviewPane'
import { TerminalPane } from './TerminalPane'

type CoreEvent = bridge.Event

const LIVE_STATES: bridge.TaskState[] = ['starting', 'queued', 'working', 'waiting']

export function App() {
  const [projects, setProjects] = createSignal<bridge.ProjectSnapshot[]>([])
  const [selectedProjectId, setSelectedProjectId] = createSignal<string | null>(null)
  const [tasks, setTasks] = createSignal<bridge.TaskSummary[]>([])
  const [selectedTaskId, setSelectedTaskId] = createSignal<string | null>(null)
  const [workspace, setWorkspace] = createSignal<bridge.TaskWorkspace | null>(null)
  const [showArchived, setShowArchived] = createSignal(false)
  const visibleTasks = () => tasks().filter((task) => showArchived() || task.state !== 'done')
  const [inspectorTab, setInspectorTab] = createSignal('terminals')
  const [prompt, setPrompt] = createSignal('')
  const [pickerOpen, setPickerOpen] = createSignal(false)
  const [scan, setScan] = createSignal<bridge.ProjectScan | null>(null)
  const [scanning, setScanning] = createSignal(false)
  const [scanError, setScanError] = createSignal<string | null>(null)
  const [error, setError] = createSignal<string | null>(null)
  const [pending, setPending] = createSignal(false)
  const [quitRequired, setQuitRequired] = createSignal<number | null>(null)
  const [shutdownNote, setShutdownNote] = createSignal<string | null>(null)

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
    setTasks(list.tasks)
    return list.tasks
  }

  async function selectTask(taskId: string) {
    setSelectedTaskId(taskId)
    setWorkspace(await bridge.taskWorkspace(taskId))
  }

  async function selectProject(project: bridge.ProjectSnapshot) {
    setSelectedProjectId(project.id)
    setSelectedTaskId(null)
    setWorkspace(null)
    setTasks([])
    const listed = await loadTasks(project.id)
    const lastTask = project.lastTaskId
      ? listed.find((task) => task.id === project.lastTaskId)
      : undefined
    if (lastTask && (showArchived() || lastTask.state !== 'done')) await selectTask(lastTask.id)
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
      case 'error':
        setError(event.data.message)
        break
      case 'shutdownStateChanged':
        if (event.data === 'closed') setShutdownNote('core stopped')
        break
    }
  }

  onMount(() => {
    void run(async () => {
      const list = await bridge.projectList()
      setProjects(list.projects)
      if (list.projects[0]) await selectProject(list.projects[0])
      await bridge.subscribeToEvents(handleEvent)
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

  function addProject(path: string) {
    setPickerOpen(false)
    void run(async () => {
      const list = await bridge.addProject(path)
      setProjects(list.projects)
      const added = list.projects.find((project) => project.path === path)
      const target = added ?? list.projects[list.projects.length - 1]
      if (target) await selectProject(target)
    })
  }

  function newTask() {
    const projectId = selectedProjectId()
    if (!projectId) return
    void run(async () => {
      const draft = await bridge.createDraft(projectId, 'New task')
      setWorkspace(draft)
      setSelectedTaskId(draft.task.id)
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
      await getCurrentWindow().close()
    })
  }

  return (
    <div class="shell">
      <div class="shell__body">
        <aside class="sidebar" aria-label="Projects">
          <h2 class="sidebar__title">Projects</h2>
          <div class="add-project">
            <button type="button" class="add-project__open" onClick={openPicker} disabled={pending()}>
              Add project
            </button>
          </div>

          <Show when={projects().length === 0}>
            <p class="empty">No projects yet. Add one with the button above.</p>
          </Show>

          <ul class="projects">
            <For each={projects()}>
              {(project) => (
                <li>
                  <button
                    type="button"
                    class="project"
                    aria-expanded={project.id === selectedProjectId()}
                    aria-current={project.id === selectedProjectId() ? 'true' : undefined}
                    onClick={() => void run(() => selectProject(project))}
                  >
                    <span class="project__name">{project.name}</span>
                    <span class="project__count">{project.unfinishedTaskCount}</span>
                  </button>
                  <Show when={project.missing}>
                    <p class="project__missing">path is missing</p>
                  </Show>

                  <Show when={project.id === selectedProjectId()}>
                    <div class="tasks">
                      <button
                        type="button"
                        class="tasks__new"
                        onClick={newTask}
                        disabled={pending()}
                      >
                        New task
                      </button>
                      <label>
                        <input type="checkbox" checked={showArchived()} onChange={(event) => setShowArchived(event.currentTarget.checked)} />
                        Show archived
                      </label>
                      <Show when={visibleTasks().length === 0}>
                        <p class="empty empty--tight">No tasks yet.</p>
                      </Show>
                      <ul class="task-list">
                        <For each={visibleTasks()}>
                          {(task) => (
                            <li>
                              <button
                                type="button"
                                class={`task task--${task.state}`}
                                aria-current={task.id === selectedTaskId() ? 'true' : undefined}
                                onClick={() => void run(() => selectTask(task.id))}
                              >
                                <span class="task__state" aria-hidden="true" />
                                <span class="task__title">{task.title}</span>
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
        </aside>

        <section class="chat" aria-label="Task chat">
          <header class="chat__header">
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
            <Show when={inspectorTab() === 'terminals'}>
              <Show when={selectedProjectId()} fallback={<p class="empty">No terminals yet.</p>}>
                {(projectId) => <TerminalPane projectId={projectId()} />}
              </Show>
            </Show>
            <Show when={inspectorTab() === 'review'}>
              <Show when={workspace()?.task.id} keyed fallback={<p class="empty">Select a task to review.</p>}>
                {(taskId) => <ReviewPane
                  task={workspace()!.task}
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
        <button type="button" class="status-bar__quit" onClick={quit} disabled={pending()}>
          Quit
        </button>
      </footer>
    </div>
  )
}

interface InspectorTab {
  id: string
  label: string
  enabled: boolean
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
