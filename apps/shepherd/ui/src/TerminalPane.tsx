/**
 * The Terminals tab. One canvas per live terminal draws the cell grid the engine
 * sends; keys are encoded here and written straight to the PTY. Ended terminals
 * stay listed with their exit code instead of pretending to still be live.
 */

import { For, Show, batch, createEffect, createMemo, createResource, createSignal, createUniqueId, onCleanup, onMount } from 'solid-js'

import * as bridge from './bridge'
import { FreebuffMark } from './FreebuffMark'
import { PickerMenu } from './PickerMenu'
import { SegmentedControl } from './SegmentedControl'
import { KITTY_REPORT_EVENTS, encodeKittyKey, encodeLegacyKey, encodeTerminalText } from './terminal_keys'
import { reportError } from './diagnostics'

const TERMINAL_TYPES: { value: 'both' | 'project' | 'task'; label: string }[] = [
  { value: 'both', label: 'Both' },
  { value: 'project', label: 'Project' },
  { value: 'task', label: 'Task' },
]

interface TerminalSize {
  columns: number
  rows: number
}

const DEFAULT_TERMINAL_SIZE: TerminalSize = { columns: 80, rows: 24 }

function RestartButton(props: { disabled: boolean; onClick: () => void; buttonRef?: (button: HTMLButtonElement) => void }) {
  const tooltipId = createUniqueId()
  return <span class="terminal-restart">
    <button type="button" ref={props.buttonRef} class="terminal-restart__button" aria-label="Restart" aria-describedby={tooltipId} disabled={props.disabled} onClick={props.onClick}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M19 8v4h-4" /><path d="M19 12a7 7 0 1 0-5.2 6.76" /><path d="m17 16 4 4m0-4-4 4" /></svg>
    </button>
    <span id={tooltipId} role="tooltip" class="chat__menu-card terminal-restart__tooltip">Restart</span>
  </span>
}

function CloseTerminalButton(props: { disabled: boolean; live?: boolean; onClick: () => void }) {
  const tooltipId = createUniqueId()
  return <span class="terminal-close">
    <button type="button" class="terminal-close__button" aria-label="Close terminal" aria-describedby={tooltipId} data-terminal-focus={props.live ? '' : undefined} disabled={props.disabled} onClick={props.onClick}>
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18" /></svg>
    </button>
    <span id={tooltipId} role="tooltip" class="chat__menu-card terminal-close__tooltip">Close terminal</span>
  </span>
}

function TerminalActionsMenu(props: {
  title: string
  cwd: string
  disabled: boolean
  contextTarget: () => HTMLElement
  contextOnly?: boolean
  onShare?: () => void
}) {
  return <PickerMenu label={`Actions for ${props.title}`} triggerClass="terminal-actions__button" align="end"
    disabled={props.disabled} contextTarget={props.contextTarget} contextOnly={props.contextOnly}
    options={[
      ...(props.onShare ? [{ id: 'share', name: 'Share', detail: 'Share this terminal with the project and keep its working directory' }] : []),
      { id: 'copy-directory', name: 'Copy working directory' },
    ]}
    onChange={(action) => {
      if (action === 'share') props.onShare?.()
      else void navigator.clipboard.writeText(props.cwd).catch((error) => reportError('copy_terminal_directory', error))
    }}>
    <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
      <circle cx="3" cy="8" r="1.25" /><circle cx="8" cy="8" r="1.25" /><circle cx="13" cy="8" r="1.25" />
    </svg>
  </PickerMenu>
}

export function FreebuffPane(props: {
  projectName: string
  entry: bridge.TerminalSnapshot | undefined
  front: boolean
  starting: boolean
  failure: string | null
  pending: boolean
  onBackToChat: () => void
  onStart: () => void
  onRestart: () => void
  onStop: () => void
  onTerminalFocus: (id: string | null) => void
}) {
  let section: HTMLElement | undefined
  let restart: HTMLButtonElement | undefined
  const live = () => props.entry !== undefined && props.entry.endedAt === null
  const focusRequest = createMemo(() => {
    const entry = props.entry
    return entry && props.front && entry.endedAt === null ? { id: entry.id } : undefined
  })

  createEffect(() => {
    if (props.entry?.endedAt == null || !props.front) return
    // The canvas unmounts with its process, so move focus to the one action
    // that still works unless the user is busy elsewhere in the window.
    const active = document.activeElement
    if (active && active !== document.body && section && !section.contains(active)) return
    queueMicrotask(() => restart?.focus({ preventScroll: true }))
  })

  return (
    <section ref={section} class="chat__terminal" aria-label="Freebuff terminal" hidden={!props.front}>
      <div class="terminal-pane__bar chat__terminal-bar">
        <span>Freebuff · {props.projectName}</span>
        <span class="terminal-list__state">{props.entry
          ? props.entry.endedAt === null ? 'live' : `ended${formatExit(props.entry.exitCode)}`
          : props.starting ? 'opening' : 'closed'}</span>
        <button type="button" onClick={props.onBackToChat}>Back to chat</button>
        <Show when={live()}>
          <button type="button" onClick={props.onStop} disabled={props.pending}>Stop process</button>
        </Show>
        <Show when={props.entry && props.entry.endedAt !== null && !props.failure}>
          <RestartButton buttonRef={(button) => { restart = button }} disabled={props.pending} onClick={props.onRestart} />
        </Show>
      </div>
      <Show when={live() ? props.entry : undefined} keyed fallback={
        <div class="chat__terminal-state">
          <Show when={props.failure} fallback={
            <Show when={props.entry} fallback={
              <Show when={props.starting} fallback={
                <p class="empty">Freebuff is closed. Choose Freebuff (terminal) from the agent picker to open it.</p>
              }>
                <p class="empty" role="status">Opening Freebuff terminal…</p>
              </Show>
            }>
              {(entry) => <div class="terminal-ended">
                <p>Ended{formatExit(entry().exitCode)}</p>
                <p><code>{entry().command}</code> in <code>{entry().cwd}</code></p>
              </div>}
            </Show>
          }>
            {(failure) => <div class="terminal-ended">
              <p>Freebuff terminal unavailable.</p>
              <p>{failure()}</p>
              <button type="button" onClick={props.onStart} disabled={props.pending}>Retry</button>
            </div>}
          </Show>
        </div>
      }>
        {(entry) => <TerminalCanvas terminalId={entry.id} shown={props.front} focusRequest={focusRequest()} onFocus={props.onTerminalFocus} />}
      </Show>
    </section>
  )
}

interface TerminalPaneProps {
  projectId: string
  taskId: string | null
  agent?: bridge.Agent
  taskHasWorktree: boolean
  transcript: bridge.Entry[] | undefined
  agentTerminals?: bridge.AgentTerminal[]
  shown: boolean
  onCommands: (commands: TerminalCommands | undefined) => void
  taskTerminalShortcut?: string
  onShow: () => void
  onLayoutLoaded: (layout: bridge.TerminalLayout) => void
  attention: string[]
  onTerminalFocus: (id: string | null) => void
  listEvent: bridge.TerminalList | undefined
  setupTerminalRequest?: { id: string }
  launchingTaskId?: string
  freebuffFront: boolean
  freebuffAttention: boolean
  onOpenFreebuff: () => void
}

type TerminalAction = 'newSharedTerminal' | 'newTaskTerminal' | 'splitRight' | 'splitDown' | 'hideTerminal'
  | 'terminalSingle' | 'terminalColumns' | 'terminalStack' | 'terminalQuad'

export interface TerminalCommands {
  available: () => { terminalReady: boolean; terminalPane: boolean; terminalMultiple: boolean; terminalFour: boolean }
  actions: Record<TerminalAction, () => void>
}

function SetupRuns(props: { taskId: string }) {
  const [runs, { refetch }] = createResource(() => props.taskId, async (taskId) => {
    try { return { entries: await bridge.setupRuns(taskId) } }
    catch (failure) { return { entries: [], error: describe(failure) } }
  })
  onMount(() => {
    const timer = setInterval(() => { if (!runs.loading) void refetch() }, 1000)
    onCleanup(() => clearInterval(timer))
  })
  return <Show when={runs.latest?.entries.length || runs.latest?.error}>
    <details class="terminal-setup-runs" open>
      <summary>Setup runs</summary>
      <Show when={runs.latest?.error}>{(error) => <p role="alert">Could not load setup runs. {error()}</p>}</Show>
      <For each={[...(runs.latest?.entries ?? [])].reverse()}>
        {(run) => <details class="terminal-setup-run" open={run.status === 'Failed'}>
          <summary>{run.name} · {run.status}</summary>
          <code>{JSON.stringify(run.command)}</code>
          <p class="empty">{run.cwd}</p>
          <Show when={run.output}><pre tabindex={0}>{run.output}</pre></Show>
        </details>}
      </For>
    </details>
  </Show>
}

export function TerminalPane(props: TerminalPaneProps) {
  const [projectEnvironment] = createResource(() => props.shown ? props.projectId : undefined, bridge.projectEnvironment)
  const measuredSizes = new Map<string, TerminalSize>()
  const [terminalList, setTerminalList] = createSignal<bridge.TerminalList>()
  const terminals = () => terminalList()?.terminals ?? []
  const [layout, setLayout] = createSignal<bridge.TerminalLayout>({ revision: 0, root: null, inspectorTab: 'terminals' })
  const [layoutLoaded, setLayoutLoaded] = createSignal(false)
  const [selectedId, setSelectedId] = createSignal<string | null>(null)
  const [focusRequest, setFocusRequest] = createSignal<{ id: string }>()
  const [pending, setPending] = createSignal(false)
  function setError(message: string | null) {
    if (message) reportError('terminal', message)
  }
  const launchTaskId = () => props.shown ? props.launchingTaskId ?? props.taskId : undefined
  const [launches, { refetch: refreshLaunches }] = createResource(launchTaskId,
    async (taskId) => {
      try { return { taskId, entries: await bridge.agentLaunches(taskId) } }
      catch (failure) { reportError('agent_launches', failure); return { taskId, entries: [] } }
    })
  const launchEntries = () => {
    const snapshot = launches.latest
    return snapshot && snapshot.taskId === launchTaskId() ? snapshot.entries : []
  }
  const [selectedLaunch, setSelectedLaunch] = createSignal<string>()
  const visibleLaunch = () => typeShown('task')
    ? launchEntries().find((entry) => entry.id === selectedLaunch()) ?? launchEntries().at(-1)
    : undefined
  onMount(() => {
    const timer = setInterval(() => {
      if (launchTaskId() && !launches.loading) void refreshLaunches()
    }, 1000)
    onCleanup(() => clearInterval(timer))
  })
  const [hiddenAgentTerminals, setHiddenAgentTerminals] = createSignal(new Set<string>())
  const [scopeFilter, setScopeFilter] = createSignal<'both' | 'project' | 'task'>('both')
  const [terminalDrag, setTerminalDrag] = createSignal<{ source: string; target: string; edge: DropEdge }>()
  let terminalPane!: HTMLDivElement
  let dragPointer: { id: number; source: string; x: number; y: number; active: boolean } | undefined
  const scopeShown = (scope: 'project' | 'task') => scopeFilter() === 'both' || scopeFilter() === scope
  const agentTerminalIds = createMemo(() => (props.agentTerminals ?? []).map((terminal) => terminal.id))
  const agentTerminal = (id: string) => props.agentTerminals?.find((terminal) => terminal.id === id)
  const agentPaneVisible = (id: string) => !hiddenAgentTerminals().has(id)
  const setAgentPaneVisible = (id: string, visible: boolean) => {
    setHiddenAgentTerminals((hidden) => {
      const next = new Set(hidden)
      if (visible) next.delete(id)
      else next.add(id)
      return next
    })
  }
  const commands = createMemo(() => {
    const commands = new Map<string, bridge.ToolCallEntry>()
    for (const entry of props.transcript ?? []) {
      if (entry.kind.type === 'toolCall' && entry.kind.data.kind === 'command') {
        commands.set(entry.id, entry.kind.data)
      }
    }
    return commands
  })

  const freebuff = () => terminals().find((terminal) => terminal.scope === 'freebuff')
  // The Freebuff session runs at the project root, so the project type governs it.
  const typeShown = (scope: 'project' | 'task' | 'freebuff') =>
    scope === 'freebuff' ? scopeShown('project') : scopeShown(scope)
  const visible = () => terminals().filter((terminal) =>
    terminal.scope !== 'freebuff' && (terminal.scope === 'project' || terminal.taskId === props.taskId))
  // Only the list and the split tree apply the type filter; lookups stay
  // unfiltered so a filtered pane keeps its title instead of reading unavailable.
  const listed = () => visible().filter((entry) => typeShown(entry.scope))
  const terminal = (id: string) => visible().find((entry) => entry.id === id)
  const paneVisible = (id: string) => !terminals().some((entry) =>
    entry.id === id && (entry.scope === 'freebuff' || !typeShown(entry.scope)
      || (entry.scope === 'task' && entry.taskId !== props.taskId)))
  const root = () => {
    if (!layoutLoaded()) return null
    const saved = layout()
    const first = visible()[0]
    return saved.revision === 0 && !saved.root && first ? leaf(first.id) : saved.root
  }
  const active = () => {
    const id = selectedId()
    const node = root()
    return id && terminal(id) && contains(node, id) ? id : firstMatchingPane(node, (pane) => !!terminal(pane))
  }
  const attached = () => paneIds(root())
  const attachedVisible = () => attached().filter((id) => terminal(id))

  function applyTerminalList(snapshot: bridge.TerminalList) {
    if (snapshot.projectId !== props.projectId) return
    const current = terminalList()
    if (current && current.revision >= snapshot.revision) {
      if (snapshot.layout && snapshot.layout.revision > layout().revision) setLayout(snapshot.layout)
      return
    }
    const previous = current ? new Set(current.terminals.map((terminal) => terminal.id)) : undefined
    batch(() => {
      setTerminalList(snapshot)
      if (snapshot.layout && snapshot.layout.revision > layout().revision) setLayout(snapshot.layout)
    })
    if (snapshot.layout) return
    if (!previous) return
    // A shell that exits while the app runs is deleted by the core, which also
    // drops its pane; mirror that here so no unavailable placeholder appears.
    const gone = [...previous].filter((id) => !snapshot.terminals.some((terminal) => terminal.id === id))
    const node = layout().root
    if (!gone.length || !node) return
    const pruned = gone.reduce<bridge.TerminalSplit | null>((tree, id) => tree && removePane(tree, id), node)
    if (pruned !== node) setLayout({ ...layout(), root: pruned })
  }

  async function run(action: () => Promise<void>) {
    if (pending()) return
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

  async function save(update: (root: bridge.TerminalSplit | null) => bridge.TerminalSplit | null, safelyRebase = false) {
    const original = root()
    for (let attempt = 0; attempt < 3; attempt++) {
      const current = root()
      const next = update(current)
      if (next === current) return
      try {
        const saved = await bridge.saveTerminalLayout(props.projectId, { ...layout(), root: next })
        if (saved.revision >= layout().revision) setLayout(saved)
        return
      } catch (failure) {
        if ((failure as bridge.BridgeError | null)?.code !== 'terminal_layout_changed' || attempt === 2) throw failure
        const list = await bridge.terminalList(props.projectId)
        applyTerminalList(list)
        const latest = list.layout ?? await bridge.terminalLayout(props.projectId)
        if (latest.revision >= layout().revision) setLayout(latest)
        if (!safelyRebase && JSON.stringify(root()) !== JSON.stringify(original)) {
          throw new Error('The terminal layout changed while applying this action. The latest layout is now shown.')
        }
      }
    }
  }

  async function show(terminalId: string, axis: bridge.SplitAxis = 'vertical', splitFrom?: string) {
    if (!contains(root(), terminalId)) {
      const added = leaf(terminalId)
      const requestedTarget = splitFrom ?? active()
      const split = (first: bridge.TerminalSplit) => splitInHalf(axis, first, added)
      await save((previous) => {
        if (contains(previous, terminalId)) return previous
        const target = previous && requestedTarget && contains(previous, requestedTarget) ? requestedTarget : firstMatchingPane(previous, (id) => !!terminal(id))
        return previous ? (target ? replacePane(previous, target, split) : split(previous)) : added
      }, true)
    }
    setSelectedId(terminalId)
    setFocusRequest({ id: terminalId })
  }

  function share(id: string) {
    void run(async () => {
      applyTerminalList(await bridge.promoteTerminal(id))
      if (scopeFilter() === 'task') setScopeFilter('both')
    })
  }

  function detach(id: string) {
    const node = root()
    if (node) void run(() => save((current) => current && contains(current, id) ? removePane(current, id) : current, true))
  }

  function arrange(axis: bridge.SplitAxis) {
    const ids = attached()
    if (ids.length > 1) void run(() => save((current) => {
      const currentIds = paneIds(current)
      return currentIds.length > 1 ? preset(currentIds, axis) : current
    }, true))
  }

  function single() {
    const current = root()
    const id = active()
    if (!current || !id) return
    const hidden = attachedVisible().filter((pane) => pane !== id)
    void run(async () => {
      await save((node) => hidden.reduce<bridge.TerminalSplit | null>(removePane, node), true)
      setFocusRequest({ id })
    })
  }

  function quad() {
    const current = root()
    const ids = attachedVisible()
    const [topLeft, topRight, bottomLeft, bottomRight] = ids
    if (!current || ids.length !== 4) return
    const rows = splitInHalf('vertical',
      splitInHalf('horizontal', leaf(topLeft), leaf(topRight)),
      splitInHalf('horizontal', leaf(bottomLeft), leaf(bottomRight)))
    const id = active()
    void run(async () => {
      await save((node) => {
        if (!node || ids.some((pane) => !contains(node, pane))) return node
        const retained = ids.slice(1).reduce<bridge.TerminalSplit | null>(removePane, node)
        return retained ? replacePane(retained, topLeft, () => rows) : node
      }, true)
      if (id) setFocusRequest({ id })
    })
  }

  function resizeSplit(path: SplitPath, ratio: number) {
    const node = root()
    if (node) void run(() => save(() => updateSplit(node, path, ratio)))
  }

  function beginDrag(event: PointerEvent, source: string) {
    if (pending() || event.button !== 0 || !event.isPrimary || dragPointer) return
    event.preventDefault()
    dragPointer = { id: event.pointerId, source, x: event.clientX, y: event.clientY, active: false }
    terminalPane.setPointerCapture(event.pointerId)
  }

  function dragMove(event: PointerEvent) {
    const pointer = dragPointer
    if (!pointer || pointer.id !== event.pointerId) return
    if (!pointer.active && Math.hypot(event.clientX - pointer.x, event.clientY - pointer.y) < 4) return
    pointer.active = true
    event.preventDefault()
    const slot = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>('.terminal-pane__slot[data-terminal-id]')
    const target = slot?.dataset.terminalId
    if (!slot || !target || target === pointer.source || !terminalPane.contains(slot)) { setTerminalDrag(undefined); return }
    const bounds = slot.getBoundingClientRect()
    const x = (event.clientX - bounds.left) / bounds.width
    const y = (event.clientY - bounds.top) / bounds.height
    const edge: DropEdge = x < .25 ? 'left' : x > .75 ? 'right' : y < .5 ? 'top' : 'bottom'
    setTerminalDrag({ source: pointer.source, target, edge })
  }

  function finishDrag(commit: boolean) {
    const pointer = dragPointer
    if (!pointer) return
    dragPointer = undefined
    if (terminalPane.hasPointerCapture(pointer.id)) terminalPane.releasePointerCapture(pointer.id)
    const drop = terminalDrag()
    setTerminalDrag(undefined)
    if (commit && pointer.active && drop) void run(async () => {
      await save((current) => moveTerminal(current, drop.source, drop.target, drop.edge), true)
      setSelectedId(drop.source)
      setFocusRequest({ id: drop.source })
    })
  }

  function moveWithKeyboard(event: KeyboardEvent, source: string) {
    const edges: Record<string, DropEdge> = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'top', ArrowDown: 'bottom' }
    const edge = edges[event.key]
    if (!event.altKey || !edge || pending()) return
    event.preventDefault()
    const slots = [...terminalPane.querySelectorAll<HTMLElement>('.terminal-pane__slot[data-terminal-id]')]
    const origin = slots.find((slot) => slot.dataset.terminalId === source)?.getBoundingClientRect()
    if (!origin) return
    const centerX = origin.left + origin.width / 2
    const centerY = origin.top + origin.height / 2
    const candidates = slots.flatMap((slot) => {
      const id = slot.dataset.terminalId
      if (!id || id === source) return []
      const bounds = slot.getBoundingClientRect()
      const dx = bounds.left + bounds.width / 2 - centerX
      const dy = bounds.top + bounds.height / 2 - centerY
      if ((edge === 'left' && dx >= 0) || (edge === 'right' && dx <= 0) || (edge === 'top' && dy >= 0) || (edge === 'bottom' && dy <= 0)) return []
      return [{ id, distance: Math.hypot(dx, dy) }]
    }).sort((left, right) => left.distance - right.distance)
    const target = candidates[0]?.id
    if (!target) return
    void run(async () => {
      await save((current) => moveTerminal(current, source, target, edge), true)
      setSelectedId(source)
      requestAnimationFrame(() => {
        const slot = [...terminalPane.querySelectorAll<HTMLElement>('.terminal-pane__slot[data-terminal-id]')].find((element) => element.dataset.terminalId === source)
        slot?.querySelector<HTMLButtonElement>('.terminal-drag-handle')?.focus({ preventScroll: true })
      })
    })
  }

  function start(taskId: string | null, axis: bridge.SplitAxis, splitFrom?: string) {
    const scope = taskId === null ? 'project' : 'task'
    // Starting a terminal the filter hides would look like nothing happened.
    if (!typeShown(scope)) setScopeFilter('both')
    const activeId = active()
    const size = (activeId && measuredSizes.get(activeId)) || DEFAULT_TERMINAL_SIZE
    const columns = activeId && axis === 'horizontal' ? Math.max(2, Math.floor(size.columns / 2)) : size.columns
    const rows = activeId && axis === 'vertical' ? Math.max(1, Math.floor(size.rows / 2)) : size.rows
    void run(async () => {
      const before = new Set(terminals().map((terminal) => terminal.id))
      const list = await bridge.startTerminal(props.projectId, taskId, columns, rows)
      applyTerminalList(list)
      const created = list.terminals.find((terminal) => !before.has(terminal.id))
      if (created) await show(created.id, axis, splitFrom)
    })
  }

  function startTaskTerminal() {
    const taskId = props.taskId
    if (!taskId || !props.taskHasWorktree || pending()) return
    props.onShow()
    start(taskId, 'vertical')
  }

  function runProjectAction(action: bridge.ProjectAction) {
    const taskId = props.taskId
    if (!taskId || !props.taskHasWorktree || pending()) return
    if (!typeShown('task')) setScopeFilter('both')
    void run(async () => {
      const before = new Set(terminals().map((terminal) => terminal.id))
      const list = await bridge.startTerminal(props.projectId, taskId, 80, 24)
      applyTerminalList(list)
      const created = list.terminals.find((terminal) => !before.has(terminal.id))
      if (!created) throw new Error('The action terminal was not created.')
      await show(created.id)
      const quoted = `'${action.command.replace(/'/g, `'\\''`)}'`
      await bridge.terminalWrite(created.id, `sh -c ${quoted}\n`)
    })
  }

  createEffect(() => {
    const event = props.listEvent
    if (event) applyTerminalList(event)
  })

  let shownSetupTerminal: { id: string } | undefined
  createEffect(() => {
    const request = props.setupTerminalRequest
    if (!request || request === shownSetupTerminal || !props.shown) return
    if (launchEntries().some((entry) => entry.id === request.id)) {
      shownSetupTerminal = request
      if (!typeShown('task')) setScopeFilter('both')
      setSelectedLaunch(request.id)
      setFocusRequest(request)
      return
    }
    if (!layoutLoaded() || pending()) return
    if (!terminals().some((entry) => entry.id === request.id && entry.taskId === props.taskId)) return
    shownSetupTerminal = request
    if (!typeShown('task')) setScopeFilter('both')
    void run(() => show(request.id))
  })

  function restart(id: string) {
    const { columns, rows } = measuredSizes.get(id) ?? DEFAULT_TERMINAL_SIZE
    void run(async () => {
      applyTerminalList(await bridge.restartTerminal(id, columns, rows))
      setSelectedId(id)
      if (props.shown && terminal(id)?.endedAt === null) setFocusRequest({ id })
    })
  }

  function splitActive(axis: bridge.SplitAxis) {
    const id = active()
    const entry = id && terminal(id)
    if (entry) start(entry.taskId, axis)
  }

  function splitPane(id: string, axis: bridge.SplitAxis) {
    const entry = terminal(id)
    if (entry) start(entry.taskId, axis, id)
  }

  function detachActive() {
    const id = active()
    if (id) detach(id)
  }

  function startSharedTerminal() {
    props.onShow()
    start(null, 'vertical')
  }

  onMount(() => {
    const cancelDrag = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && dragPointer) { event.preventDefault(); finishDrag(false) }
    }
    window.addEventListener('keydown', cancelDrag)
    onCleanup(() => window.removeEventListener('keydown', cancelDrag))
    const terminalReady = () => layoutLoaded() && !pending()
    props.onCommands({
      available: () => ({
        terminalReady: terminalReady(),
        terminalPane: terminalReady() && props.shown && !!active(),
        terminalMultiple: terminalReady() && props.shown && attached().length > 1,
        terminalFour: terminalReady() && props.shown && attachedVisible().length === 4,
      }),
      actions: {
        newSharedTerminal: startSharedTerminal,
        newTaskTerminal: startTaskTerminal,
        splitRight: () => splitActive('horizontal'),
        splitDown: () => splitActive('vertical'),
        hideTerminal: detachActive,
        terminalSingle: single,
        terminalColumns: () => arrange('horizontal'),
        terminalStack: () => arrange('vertical'),
        terminalQuad: quad,
      },
    })
    onCleanup(() => props.onCommands(undefined))
    let disposed = false
    void run(async () => {
      const [list, saved] = await Promise.all([bridge.terminalList(props.projectId), bridge.terminalLayout(props.projectId)])
      if (disposed) return
      batch(() => {
        applyTerminalList(list)
        const latest = [saved, ...(list.layout ? [list.layout] : []), layout()].reduce((current, candidate) => candidate.revision > current.revision ? candidate : current)
        setLayout(latest)
        props.onLayoutLoaded(latest)
        setLayoutLoaded(true)
      })
    })
    onCleanup(() => { disposed = true })
  })

  return (
    <div ref={terminalPane} class="terminal-pane" onPointerMove={dragMove}
      onPointerUp={(event) => { if (dragPointer?.id === event.pointerId) finishDrag(true) }}
      onPointerCancel={(event) => { if (dragPointer?.id === event.pointerId) finishDrag(false) }}
      onLostPointerCapture={(event) => { if (dragPointer?.id === event.pointerId) finishDrag(false) }}>
      <div class="terminal-pane__heading">
        <h2>Terminals</h2>
        <SegmentedControl label="Terminal types" value={scopeFilter()} choices={TERMINAL_TYPES}
          onChange={setScopeFilter} />
        <button type="button" class="terminal-pane__new" aria-label="New terminal"
          title={scopeFilter() === 'project' ? 'New project terminal' : 'New task terminal'}
          disabled={pending() || (scopeFilter() !== 'project' && !props.taskHasWorktree)}
          onClick={() => scopeFilter() === 'project' ? startSharedTerminal() : startTaskTerminal()}>
          <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M11 16H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v6M5 7l3 2.5L5 12m5 0h2" />
            <path d="M15.5 12.5v6m-3-3h6" />
          </svg>
        </button>
      </div>
      <Show when={projectEnvironment.latest?.current?.actions.length}>
        <div aria-label="Project actions">
          <For each={projectEnvironment.latest?.current?.actions}>{(action) => <button class="project-action" type="button"
            disabled={!props.taskHasWorktree || pending() || !action.command.trim()} onClick={() => runProjectAction(action)}>{action.name}</button>}</For>
        </div>
      </Show>
        <ul class="terminal-list" aria-label="Terminals">
        <For each={listed()}>
          {(entry) => {
            let tab!: HTMLButtonElement
            return <li class="terminal-list__row">
              <button ref={tab} type="button" class="terminal-list__item"
                aria-current={entry.id === active() ? 'true' : undefined}
                title={`${entry.scope === 'project' ? 'Project shared' : 'Task'} terminal · ${entry.endedAt === null ? 'live' : 'ended'}${contains(root(), entry.id) ? '' : ' · view closed'}`}
                disabled={pending()} onClick={() => void run(() => show(entry.id))}>
                <Show when={entry.scope === 'project'}><ProjectTerminalIcon /></Show>
                <span>{entry.title}</span>
                <Show when={props.attention.includes(entry.id)}>
                  <span class="terminal-attention" role="img" aria-label="Terminal needs attention" />
                </Show>
              </button>
              <TerminalActionsMenu title={entry.title} cwd={entry.cwd} disabled={pending()} contextTarget={() => tab}
                contextOnly onShare={entry.scope === 'task' ? () => share(entry.id) : undefined} />
            </li>
          }}
        </For>
        <Show when={typeShown('freebuff') ? freebuff() : undefined}>
          {(entry) => (
            <li class="terminal-list__row">
              <button type="button" class="terminal-list__item" aria-current={props.freebuffFront ? 'true' : undefined}
                title={`Freebuff · ${entry().endedAt === null ? 'live' : 'ended'}${props.freebuffFront ? ' · in front' : ''}`}
                onClick={props.onOpenFreebuff}>
                <FreebuffMark size={13} />
                <span>Freebuff</span>
                <Show when={props.freebuffAttention}>
                  <span class="terminal-attention" role="img" aria-label="Freebuff terminal needs attention" />
                </Show>
              </button>
            </li>
          )}
        </Show>
        <For each={typeShown('task') ? props.agentTerminals?.filter((entry) => agentPaneVisible(entry.id)) : []}>
          {(entry) => (
            <li>
              <button type="button" class="terminal-list__item" onClick={() => {
                setAgentPaneVisible(entry.id, true)
                if (entry.live) setFocusRequest({ id: entry.id })
              }}>
                <span>agent: {entry.title}</span>
              </button>
            </li>
          )}
        </For>
        <For each={typeShown('task') ? launchEntries().map((entry) => entry.id) : []}>
          {(id) => <Show when={launchEntries().find((entry) => entry.id === id)}>
            {(entry) => {
              let tab!: HTMLButtonElement
              return <li class="terminal-list__row">
                <button ref={tab} type="button" class="terminal-list__item"
                  aria-current={id === visibleLaunch()?.id ? 'true' : undefined}
                  onClick={() => { setSelectedLaunch(id); if (entry().live) setFocusRequest({ id }) }}>
                  <span>{entry().setupTitle ?? 'Agent launch'}</span>
                  <span class="terminal-list__state">{entry().live ? 'Running' : 'Ended'}</span>
                </button>
                <TerminalActionsMenu title={entry().setupTitle ?? 'Agent launch'} cwd={entry().cwd}
                  disabled={pending()} contextOnly contextTarget={() => tab} />
              </li>
            }}
          </Show>}
        </For>
        </ul>
      <Show when={typeShown('task') && props.shown && props.taskHasWorktree ? props.taskId : undefined} keyed>
        {(taskId) => <SetupRuns taskId={taskId} />}
      </Show>
      <Show when={visibleLaunch()}>
        {(entry) => {
          let header!: HTMLDivElement
          return <section class="terminal-tile terminal-agent-launch" aria-label={entry().setupTitle ?? 'Agent launch terminal'}>
            <div ref={header} class="terminal-pane__bar">
              <span>{entry().setupTitle ?? 'Agent launch'}</span>
              <div class="terminal-header__actions">
                <TerminalActionsMenu title={entry().setupTitle ?? 'Agent launch'} cwd={entry().cwd} disabled={pending()} contextTarget={() => header} />
                <Show when={entry().setupTitle && entry().live}>
                  <CloseTerminalButton disabled={pending()} live
                    onClick={() => void run(async () => { await bridge.closeTerminal(entry().id); await refreshLaunches() })} />
                </Show>
              </div>
            </div>
            <Show when={entry().command}><code class="terminal-agent-launch__command" title={entry().cwd}>{entry().command}</code></Show>
            <TerminalCanvas terminalId={entry().id} shown={props.shown} readOnly={!entry().live}
              focusRequest={focusRequest()} onFocus={props.onTerminalFocus} />
          </section>
        }}
      </Show>
      <Show when={props.taskId} keyed>
        <Show when={commands().size > 0}>
          <CommandTerminals commands={commands()} shown={props.shown} agent={props.agent} />
        </Show>
      </Show>
      <For each={typeShown('task') ? agentTerminalIds() : []}>
        {(id) => (
          <Show when={agentPaneVisible(id) && agentTerminal(id)}>
            {(entry) => (
              <section class="terminal-tile" data-agent-terminal aria-label={`agent: ${entry().title}`}>
                <div class="terminal-pane__bar">
                  <span>agent: {entry().title}</span>
                  <div class="terminal-header__actions"><CloseTerminalButton live={entry().live} disabled={pending()} onClick={() => void run(async () => {
                    if (entry().live) await bridge.closeTerminal(id)
                    setAgentPaneVisible(id, false)
                  })} /></div>
                </div>
                <Show when={entry().live} fallback={
                  <div class="terminal-ended"><p>Process ended{formatExit(entry().exitCode)}</p></div>
                }>
                  <TerminalCanvas terminalId={id} shown={props.shown} focusRequest={focusRequest()} onFocus={props.onTerminalFocus} />
                </Show>
              </section>
            )}
          </Show>
        )}
      </For>
      <Show when={firstMatchingPane(root(), paneVisible)} fallback={
        <Show when={commands().size === 0 && !agentTerminalIds().some(agentPaneVisible)}>
          <p class="empty">Open an instance or start a terminal.</p>
        </Show>
      }>
        <div class="terminal-tree-root">
          <TerminalTree node={root()!} terminal={terminal} paneVisible={paneVisible} shown={props.shown} select={setSelectedId} focusRequest={focusRequest()} onTerminalFocus={props.onTerminalFocus}
            restart={restart} measure={(id, size) => measuredSizes.set(id, size)} path={[]} resize={resizeSplit} detach={detach} stop={(id) => void run(() => bridge.closeTerminal(id))} split={splitPane} beginDrag={beginDrag} moveWithKeyboard={moveWithKeyboard} pending={pending()}
            share={share} />
          <Show when={terminalDrag()}>{(drop) => <div class="terminal-layout-preview" aria-hidden="true"><TerminalLayoutPreview node={moveTerminal(root(), drop().source, drop().target, drop().edge)} source={drop().source} paneVisible={paneVisible} /></div>}</Show>
        </div>
      </Show>
    </div>
  )
}

function CommandTerminals(props: { commands: ReadonlyMap<string, bridge.ToolCallEntry>; shown: boolean; agent?: bridge.Agent }) {
  const agentName = () => props.agent ? bridge.AGENT_LABELS[props.agent] : 'Agent'
  const [selection, setSelection] = createSignal<string>()
  const ids = createMemo(() => Array.from(props.commands.keys()))
  const selectedId = () => {
    const id = selection()
    return id && props.commands.has(id) ? id : ids().at(-1)
  }

  return (
    <section class="terminal-tile terminal-tile--commands" aria-label={`${agentName()} command terminals`}>
      <div class="terminal-pane__bar">
        <span>{agentName()} commands</span>
        <span class="terminal-list__state">Read-only</span>
      </div>
      <ul class="terminal-list" aria-label={`${agentName()} commands`}>
        <For each={ids()}>
          {(id) => (
            <li>
              <button type="button" class="terminal-list__item" aria-current={id === selectedId() ? 'true' : undefined}
                onClick={() => setSelection(id)}>
                <span>{props.commands.get(id)?.title}</span>
                <span class="terminal-list__state">{props.commands.get(id)?.status}</span>
              </button>
            </li>
          )}
        </For>
      </ul>
      <Show when={selectedId()} keyed>
        {(id) => <CommandOutput text={props.commands.get(id)?.output} shown={props.shown} />}
      </Show>
    </section>
  )
}

function CommandOutput(props: { text: string | null | undefined; shown: boolean }) {
  let output: HTMLPreElement | undefined
  let follow = true

  createEffect(() => {
    const text = props.text
    if (output && text && props.shown && follow) output.scrollTop = output.scrollHeight
  })

  return (
    <pre class="terminal-command-output" ref={output} tabIndex={0} aria-label="Command output"
      onScroll={(event) => {
        const { scrollHeight, scrollTop, clientHeight } = event.currentTarget
        follow = scrollHeight - scrollTop - clientHeight <= 1
      }}>{props.text ?? 'No output yet.'}</pre>
  )
}

function ProjectTerminalIcon() {
  return (
    <svg class="terminal-scope-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </svg>
  )
}

function PlusIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
      <path d="M12 5v14M5 12h14" />
    </svg>
  )
}

function leaf(terminalId: string): bridge.TerminalSplit {
  return { type: 'pane', terminalId }
}

function splitInHalf(axis: bridge.SplitAxis, first: bridge.TerminalSplit, second: bridge.TerminalSplit): bridge.TerminalSplit {
  return { type: 'split', axis, ratio: 500, first, second }
}

function contains(node: bridge.TerminalSplit | null, terminalId: string): boolean {
  if (!node) return false
  return node.type === 'pane' ? node.terminalId === terminalId
    : contains(node.first, terminalId) || contains(node.second, terminalId)
}

function replacePane(node: bridge.TerminalSplit, id: string, replace: (pane: bridge.TerminalSplit) => bridge.TerminalSplit): bridge.TerminalSplit {
  if (node.type === 'pane') return node.terminalId === id ? replace(node) : node
  return { ...node, first: replacePane(node.first, id, replace), second: replacePane(node.second, id, replace) }
}

function removePane(node: bridge.TerminalSplit | null, id: string): bridge.TerminalSplit | null {
  if (!node) return null
  if (node.type === 'pane') return node.terminalId === id ? null : node
  const first = removePane(node.first, id)
  const second = removePane(node.second, id)
  return first && second ? { ...node, first, second } : first ?? second
}

type DropEdge = 'left' | 'right' | 'top' | 'bottom'

function moveTerminal(node: bridge.TerminalSplit | null, source: string, target: string, edge: DropEdge): bridge.TerminalSplit | null {
  if (!node || source === target || !contains(node, source) || !contains(node, target)) return node
  const remaining = removePane(node, source)
  if (!remaining || !contains(remaining, target)) return node
  const axis = edge === 'left' || edge === 'right' ? 'horizontal' : 'vertical'
  return replacePane(remaining, target, (pane) => edge === 'left' || edge === 'top'
    ? splitInHalf(axis, leaf(source), pane) : splitInHalf(axis, pane, leaf(source)))
}

function TerminalLayoutPreview(props: { node: bridge.TerminalSplit | null; source: string; paneVisible: (id: string) => boolean }) {
  const pane = () => props.node?.type === 'pane' ? props.node : undefined
  const split = () => props.node?.type === 'split' ? props.node : undefined
  return <Show when={pane()} fallback={<Show when={split()}>{(branch) => <div class={`terminal-split terminal-split--${branch().axis}`}>
    <Show when={firstMatchingPane(branch().first, props.paneVisible)}><div class="terminal-split__child" style={{ flex: branch().ratio }}><TerminalLayoutPreview node={branch().first} source={props.source} paneVisible={props.paneVisible} /></div></Show>
    <Show when={firstMatchingPane(branch().first, props.paneVisible) && firstMatchingPane(branch().second, props.paneVisible)}><div class="terminal-divider terminal-preview__divider" /></Show>
    <Show when={firstMatchingPane(branch().second, props.paneVisible)}><div class="terminal-split__child" style={{ flex: 1000 - branch().ratio }}><TerminalLayoutPreview node={branch().second} source={props.source} paneVisible={props.paneVisible} /></div></Show>
  </div>}</Show>}>
    {(entry) => <div class="terminal-pane__slot"><div class="terminal-tile terminal-preview__tile" classList={{ 'terminal-preview__tile--target': entry().terminalId === props.source }} /></div>}
  </Show>
}

function paneIds(node: bridge.TerminalSplit | null): string[] {
  if (!node) return []
  return node.type === 'pane' ? [node.terminalId] : [...paneIds(node.first), ...paneIds(node.second)]
}

function preset(ids: string[], axis: bridge.SplitAxis): bridge.TerminalSplit | null {
  if (!ids.length) return null
  const [firstId] = ids
  if (ids.length === 1) return leaf(firstId)
  const midpoint = Math.floor(ids.length / 2)
  const first = preset(ids.slice(0, midpoint), axis)
  const second = preset(ids.slice(midpoint), axis)
  if (!first || !second) return first ?? second
  return { type: 'split', axis, ratio: Math.round(midpoint / ids.length * 1000), first, second }
}

type TerminalLookup = (id: string) => bridge.TerminalSnapshot | undefined

function firstMatchingPane(node: bridge.TerminalSplit | null, matches: (id: string) => boolean): string | null {
  if (!node) return null
  return node.type === 'pane'
    ? (matches(node.terminalId) ? node.terminalId : null)
    : firstMatchingPane(node.first, matches) ?? firstMatchingPane(node.second, matches)
}

type SplitSide = 'first' | 'second'
type SplitPath = readonly SplitSide[]

function updateSplit(node: bridge.TerminalSplit, path: SplitPath, ratio: number): bridge.TerminalSplit {
  if (node.type !== 'split') return node
  const [side, ...rest] = path
  return side ? { ...node, [side]: updateSplit(node[side], rest, ratio) } : { ...node, ratio }
}

interface TerminalTreeProps {
  node: bridge.TerminalSplit
  path: SplitPath
  resize: (path: SplitPath, ratio: number) => void
  terminal: TerminalLookup
  paneVisible: (id: string) => boolean
  shown: boolean
  select: (id: string) => void
  onTerminalFocus: (id: string | null) => void
  focusRequest: { id: string } | undefined
  restart: (id: string) => void
  measure: (id: string, size: TerminalSize) => void
  stop: (id: string) => void
  share: (id: string) => void
  detach: (id: string) => void
  split: (id: string, axis: bridge.SplitAxis) => void
  beginDrag: (event: PointerEvent, id: string) => void
  moveWithKeyboard: (event: KeyboardEvent, id: string) => void
  pending: boolean
}

function TerminalTree(props: TerminalTreeProps) {
  const [preview, setPreview] = createSignal<number | null>(null)
  let header!: HTMLDivElement
  let bounds: DOMRect | undefined
  let dragPointer: number | undefined

  function ratioAt(event: PointerEvent, axis: bridge.SplitAxis) {
    if (!bounds) return null
    const horizontal = axis === 'horizontal'
    const position = horizontal ? event.clientX - bounds.left : event.clientY - bounds.top
    const size = horizontal ? bounds.width : bounds.height
    return size > 0 ? Math.max(100, Math.min(900, Math.round(position / size * 1000))) : null
  }

  const split = () => props.node.type === 'split' ? props.node : null
  const pane = () => props.node.type === 'pane' ? props.terminal(props.node.terminalId) : undefined
  return (
    <Show when={split()} fallback={
      <Show when={pane()} fallback={
        <Show when={props.node.type === 'pane' ? props.node.terminalId : undefined} keyed>
          {(id) => (
            <section class="terminal-tile" aria-label="Unavailable terminal">
              <div class="terminal-pane__bar">
                <span>Unavailable terminal</span>
                <div class="terminal-header__actions"><CloseTerminalButton disabled={props.pending} onClick={() => props.detach(id)} /></div>
              </div>
              <div class="terminal-ended">
                <p>The saved terminal no longer exists.</p>
              </div>
            </section>
          )}
        </Show>
      }>
        {(entry) => (
          <div class="terminal-pane__slot" data-terminal-id={entry().id}>
          <section class="terminal-tile"
            aria-label={entry().title} aria-description={entry().scope === 'project' ? 'Shared terminal' : 'Task terminal'}
            onFocusIn={() => props.select(entry().id)}>
            <div ref={header} class="terminal-pane__bar">
              <button type="button" class="terminal-drag-handle" aria-label={`Rearrange ${entry().title}`} title="Drag to rearrange, or use Alt and the arrow keys" disabled={props.pending} onPointerDown={(event) => props.beginDrag(event, entry().id)} onKeyDown={(event) => props.moveWithKeyboard(event, entry().id)}>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="9" cy="5" r="1.5" /><circle cx="15" cy="5" r="1.5" /><circle cx="9" cy="12" r="1.5" /><circle cx="15" cy="12" r="1.5" /><circle cx="9" cy="19" r="1.5" /><circle cx="15" cy="19" r="1.5" /></svg>
              </button>
              <Show when={entry().scope === 'project'}><ProjectTerminalIcon /></Show>
              <span>{entry().title}</span>
              <div class="terminal-header__actions">
                <TerminalActionsMenu title={entry().title} cwd={entry().cwd} disabled={props.pending} contextTarget={() => header}
                  onShare={entry().scope === 'task' ? () => props.share(entry().id) : undefined} />
                <Show when={entry().endedAt !== null}><RestartButton disabled={props.pending} onClick={() => props.restart(entry().id)} /></Show>
                <CloseTerminalButton live={entry().endedAt === null} disabled={props.pending} onClick={() => props.stop(entry().id)} />
              </div>
            </div>
            <Show when={entry().endedAt === null || entry().id.startsWith('shepherd-preview:')} fallback={
              <div class="terminal-ended">
                <p>Ended{formatExit(entry().exitCode)}</p>
                <p><code>{entry().command}</code> in <code>{entry().cwd}</code></p>
              </div>
            }>
              <Show when={entry().endedAt !== null}><p class="empty">Ended{formatExit(entry().exitCode)}</p></Show>
              <Show when={entry().id} keyed>
                {(id) => <TerminalCanvas terminalId={id} shown={props.shown} readOnly={entry().endedAt !== null} focusRequest={props.focusRequest} onFocus={props.onTerminalFocus} onMeasure={(size) => props.measure(id, size)} />}
              </Show>
            </Show>
          </section>
          <button type="button" class="terminal-split-add terminal-split-add--right" aria-label="Split right"
            title="Split right" tabIndex={-1} disabled={props.pending}
            onClick={() => props.split(entry().id, 'horizontal')}>
            <PlusIcon />
          </button>
          <button type="button" class="terminal-split-add terminal-split-add--bottom" aria-label="Split down"
            title="Split down" tabIndex={-1} disabled={props.pending}
            onClick={() => props.split(entry().id, 'vertical')}>
            <PlusIcon />
          </button>
          </div>
        )}
      </Show>
    }>
      {(branch) => (
        <div class={`terminal-split terminal-split--${branch().axis}`}>
          <Show when={firstMatchingPane(branch().first, props.paneVisible)}>
            <div class="terminal-split__child" style={{ flex: preview() ?? branch().ratio }}>
              <TerminalTree {...props} node={branch().first} path={[...props.path, 'first']} />
            </div>
          </Show>
          <Show when={firstMatchingPane(branch().first, props.paneVisible) && firstMatchingPane(branch().second, props.paneVisible)}>
            <div class="terminal-divider" role="separator" tabIndex={0} aria-label="Resize terminal split"
              aria-orientation={branch().axis === 'horizontal' ? 'vertical' : 'horizontal'}
              aria-valuemin={10} aria-valuemax={90} aria-valuenow={Math.round((preview() ?? branch().ratio) / 10)}
              onPointerDown={(event) => {
                if (event.button !== 0 || props.pending) return
                bounds = event.currentTarget.parentElement?.getBoundingClientRect()
                dragPointer = event.pointerId
                event.currentTarget.setPointerCapture(event.pointerId)
              }}
              onPointerMove={(event) => {
                if (event.pointerId === dragPointer) setPreview(ratioAt(event, branch().axis))
              }}
              onPointerUp={(event) => {
                if (event.pointerId !== dragPointer) return
                const ratio = ratioAt(event, branch().axis)
                dragPointer = undefined
                setPreview(null)
                if (ratio !== null && !props.pending) props.resize(props.path, ratio)
              }}
              onLostPointerCapture={() => { dragPointer = undefined; setPreview(null) }}
              onKeyDown={(event) => {
                const keys = branch().axis === 'horizontal' ? ['ArrowLeft', 'ArrowRight'] : ['ArrowUp', 'ArrowDown']
                const direction = keys.indexOf(event.key)
                if (direction < 0 || props.pending) return
                event.preventDefault()
                props.resize(props.path, Math.max(100, Math.min(900, branch().ratio + (direction ? 50 : -50))))
              }} />
          </Show>
          <Show when={firstMatchingPane(branch().second, props.paneVisible)}>
            <div class="terminal-split__child" style={{ flex: 1000 - (preview() ?? branch().ratio) }}>
              <TerminalTree {...props} node={branch().second} path={[...props.path, 'second']} />
            </div>
          </Show>
        </div>
      )}
    </Show>
  )
}

function formatExit(exitCode: number | null): string {
  if (exitCode === null) return ' without an exit code'
  return ` with exit code ${exitCode}`
}

const FONT = '13px ui-monospace, SFMono-Regular, Menlo, monospace'
const LINE_HEIGHT = 17
const APP_CURSOR = 1 << 1
const ALT_SCREEN = 1 << 12
const ALTERNATE_SCROLL = 1 << 15
const MOUSE_CLICK = 1 << 3
const BRACKETED_PASTE = 1 << 4
const BRACKETED_PASTE_END = '\x1b[201~'
const MOUSE_SGR = 1 << 5
const MOUSE_MOTION = 1 << 6
const FOCUS_REPORTING = 1 << 11
const MOUSE_DRAG = 1 << 13
const MOUSE_UTF8 = 1 << 14
const MOUSE_REPORTING = MOUSE_CLICK | MOUSE_MOTION | MOUSE_DRAG
const MAX_PENDING_INPUT_WRITES = 128
const MAX_PENDING_INPUT_BYTES = 1024 * 1024
const INPUT_RELEASE_BUDGET = 128

function terminalPaintColors() {
  const style = getComputedStyle(document.documentElement)
  const background = style.getPropertyValue('--terminal-background').trim()
  return {
    background,
    backgroundValue: Number.parseInt(background.slice(1), 16),
    search: style.getPropertyValue('--terminal-search').trim(),
    selection: style.getPropertyValue('--terminal-selection').trim(),
    cursor: style.getPropertyValue('--terminal-cursor').trim(),
    blockCursor: style.getPropertyValue('--terminal-block-cursor').trim(),
  }
}

function TerminalCanvas(props: { terminalId: string; shown: boolean; readOnly?: boolean; focusRequest: { id: string } | undefined; onFocus?: (id: string | null) => void; onMeasure?: (size: TerminalSize) => void }) {
  const mac = navigator.platform.startsWith('Mac')
  const linux = navigator.platform.startsWith('Linux')
  function setError(message: string | null) {
    if (message) reportError('terminal', message)
  }
  let input: HTMLTextAreaElement | undefined
  const [finding, setFinding] = createSignal(false)
  const [copyText, setCopyText] = createSignal<string | null>(null)
  const [visibleText, setVisibleText] = createSignal('')
  const instructionsId = createUniqueId()
  const copyShortcut = mac ? 'Command+Shift+X' : 'Control+Shift+X'
  const [searching, setSearching] = createSignal(false)
  const [noMatch, setNoMatch] = createSignal(false)
  let searchId: number | null = null
  let searchInput: HTMLInputElement | undefined
  let copyInput: HTMLTextAreaElement | undefined
  let searchGeneration = 0
  let pendingSearch: Promise<number | null> | undefined
  let composing = false
  let committedComposition: string | undefined
  let compositionTimer: number | undefined
  let canvas: HTMLCanvasElement | undefined
  let container: HTMLDivElement | undefined
  let frame: bridge.TerminalFrame | null = null
  let paintedFrame: bridge.TerminalFrame | null = null
  let paintRequest: number | undefined
  let dirtyRows: Set<number> | null = null
  let cellWidth = 7.8
  let lastSent = ''
  let resizing = false
  let queuedSize: ReturnType<typeof measure> | undefined
  let inputWrites = Promise.resolve()
  let pendingInputWrites = 0
  let pendingInputBytes = 0
  const reportedKeys = new Map<string, (event?: KeyboardEvent) => string>()
  let selection: { start: number; end: number } | undefined
  let selecting: number | undefined
  let selectionAnchor: number | undefined
  let mouseCapture: { lastEvent: PointerEvent; cell: { column: number; row: number }; button: number } | undefined
  let mousePosition = ''
  let verticalWheelRemainder = 0
  let horizontalWheelRemainder = 0
  let reportedFocus: boolean | undefined
  let paintColors = terminalPaintColors()

  createEffect(() => {
    if (props.focusRequest?.id === props.terminalId) input?.focus({ preventScroll: true })
  })

  function scheduleDraw(rows: Iterable<number> | null = null) {
    if (rows === null) dirtyRows = null
    else if (dirtyRows) for (const row of rows) dirtyRows.add(row)
    if (paintRequest !== undefined || !props.shown || document.hidden) return
    paintRequest = window.requestAnimationFrame(() => {
      paintRequest = undefined
      const rows = dirtyRows
      dirtyRows = new Set()
      draw(rows)
    })
  }

  function syncViewVisibility() {
    if (props.shown && !document.hidden) {
      scheduleDraw()
      if (frame) setVisibleText(viewportText(frame))
    } else {
      if (paintRequest !== undefined) window.cancelAnimationFrame(paintRequest)
      paintRequest = undefined
      dirtyRows = null
    }
  }

  createEffect(() => {
    if (!props.shown) {
      leaveTerminal()
      setCopyText(null)
    }
    syncViewVisibility()
  })

  function draw(changedRows: Iterable<number> | null) {
    const current = frame
    if (!canvas || !current) return
    const context = canvas.getContext('2d')
    if (!context) return

    const ratio = window.devicePixelRatio || 1
    const width = Math.ceil(current.columns * cellWidth)
    const height = current.rows * LINE_HEIGHT
    const pixelWidth = Math.ceil(width * ratio)
    const pixelHeight = Math.ceil(height * ratio)
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
      canvas.width = pixelWidth
      canvas.height = pixelHeight
      changedRows = null
    }
    canvas.style.width = `${width}px`
    canvas.style.height = `${height}px`
    if (input) {
      const left = `${8 + current.cursorColumn * cellWidth}px`
      const top = `${8 + current.cursorRow * LINE_HEIGHT}px`
      const moved = input.style.left !== left || input.style.top !== top
      input.style.left = left
      input.style.top = top
      if (linux && moved && !composing && !input.value && document.activeElement === input) {
        // WebKitGTK reports the previous IME caret after CSS-only moves.
        input.value = ' '
        input.value = ''
      }
    }
    context.setTransform(ratio, 0, 0, ratio, 0, 0)
    context.textBaseline = 'top'

    const columns = current.columns
    const match = current.searchHighlight
    const highlight = match && match.id === searchId ? [match.start, match.end] : null
    const selected = selection ? selectionRange(current, selection) : null
    const rows = changedRows ?? Array.from({ length: current.rows }, (_, row) => row)
    for (const row of rows) {
      const y = row * LINE_HEIGHT
      const start = row * columns
      context.fillStyle = paintColors.background
      context.fillRect(0, y, width, LINE_HEIGHT)
      for (let column = 0; column < columns; column++) {
        const cell = current.cells[start + column]
        if (cell.background === paintColors.backgroundValue) continue
        context.fillStyle = hex(cell.background)
        context.fillRect(column * cellWidth, y, cellWidth, LINE_HEIGHT)
      }
      for (const [range, color] of [[highlight, paintColors.search], [selected, paintColors.selection]] as const) {
        if (!range) continue
        const first = Math.max(0, range[0] - start)
        const last = Math.min(columns - 1, range[1] - start)
        if (first > last) continue
        context.fillStyle = color
        context.fillRect(first * cellWidth, y, (last - first + 1) * cellWidth, LINE_HEIGHT)
      }
      for (let column = 0; column < columns; column++) {
        const cell = current.cells[start + column]
        if (!cell.width) continue
        const x = column * cellWidth
        const width = cell.width * cellWidth
        context.fillStyle = hex(cell.foreground)
        if (cell.text && cell.text !== ' ') {
          const font = `${cell.italic ? 'italic ' : ''}${cell.bold ? 'bold ' : ''}${FONT}`
          if (context.font !== font) context.font = font
          context.fillText(cell.text, x, y + 1, width)
        }
        if (cell.linked || cell.underline) context.fillRect(x, y + LINE_HEIGHT - 2, width, 1)
        if (cell.strikeout) context.fillRect(x, y + Math.floor(LINE_HEIGHT / 2), width, 1)
      }
      if (current.cursorVisible && row === current.cursorRow) {
        const x = current.cursorColumn * cellWidth
        context.fillStyle = paintColors.cursor
        if (current.cursorStyle === 1) context.fillRect(x, y + LINE_HEIGHT - 2, cellWidth, 2)
        else if (current.cursorStyle === 2) context.fillRect(x, y, 2, LINE_HEIGHT)
        else if (current.cursorStyle === 3) {
          context.strokeStyle = context.fillStyle
          context.strokeRect(x + 0.5, y + 0.5, cellWidth - 1, LINE_HEIGHT - 1)
        } else {
          context.fillStyle = paintColors.blockCursor
          context.fillRect(x, y, cellWidth, LINE_HEIGHT)
        }
      }
    }
    paintedFrame = current
  }

  function cellAt(event: MouseEvent, current = frame) {
    if (!current || !canvas) return null
    const bounds = canvas.getBoundingClientRect()
    return {
      column: Math.max(0, Math.min(current.columns - 1, Math.floor((event.clientX - bounds.left) / cellWidth))),
      row: Math.max(0, Math.min(current.rows - 1, Math.floor((event.clientY - bounds.top) / LINE_HEIGHT))),
    }
  }

  function reportMouse(event: MouseEvent, button: number, action: 'press' | 'release' | 'move', repeat = 1, cell = cellAt(event)): boolean | undefined {
    // Only unhandled events fall through to local selection or scrolling.
    const current = frame
    if (!current || !cell || !(current.modes & MOUSE_REPORTING)) return
    if (action !== 'release' && (current.displayOffset || event.shiftKey)) return
    const position = `${cell.column},${cell.row}`
    if (action === 'move') {
      if (!(current.modes & MOUSE_MOTION) && (!(current.modes & MOUSE_DRAG) || !mouseCapture)) return
      if (mousePosition === position) return false
      button += 32
    }
    const modifiers = (event.altKey ? 8 : 0) + (event.ctrlKey ? 16 : 0)
    const column = Math.min(cell.column + 1, current.columns)
    const row = Math.min(cell.row + 1, current.rows)
    let data: string | number[]
    if (current.modes & MOUSE_SGR) {
      data = `\x1b[<${button + modifiers};${column};${row}${action === 'release' ? 'm' : 'M'}`.repeat(repeat)
    } else {
      const utf8 = (current.modes & MOUSE_UTF8) !== 0
      const limit = utf8 ? 2015 : 223
      if (column > limit || row > limit) return false
      const code = (action === 'release' ? 3 : button) + modifiers + 32
      const prefix = [27, 91, 77, code]
      const coordinates = utf8
        ? Array.from(new TextEncoder().encode(String.fromCodePoint(column + 32, row + 32)))
        : [column + 32, row + 32]
      data = Array.from({ length: repeat }, () => [...prefix, ...coordinates]).flat()
    }
    const queued = write(data, false, action === 'press' && button < 3)
    if (queued) mousePosition = position
    return queued
  }

  function onPointerDown(event: PointerEvent & { currentTarget: HTMLCanvasElement }) {
    input?.focus({ preventScroll: true })
    const cell = cellAt(event)
    if (!cell || !frame || mouseCapture || selecting !== undefined) return
    if (event.button === 0 && (event.metaKey || event.ctrlKey) && paintedFrame) {
      const painted = cellAt(event, paintedFrame)
      if (painted && paintedFrame.cells[painted.row * paintedFrame.columns + painted.column]?.linked) {
        event.preventDefault()
        void bridge.openTerminalLink(props.terminalId, painted.row, painted.column, paintedFrame.revision)
          .catch((failure) => setError(describe(failure)))
        return
      }
    }
    const button = [0, 1, 2][event.button]
    const reported = button === undefined ? undefined : reportMouse(event, button, 'press', 1, cell)
    if (reported !== undefined) {
      event.preventDefault()
      if (reported) {
        mouseCapture = { lastEvent: event, cell, button }
        event.currentTarget.setPointerCapture(event.pointerId)
      }
      return
    }
    if (event.button !== 0) return
    event.preventDefault()
    selectionAnchor = cell.row * frame.columns + cell.column
    selection = undefined
    selecting = event.pointerId
    event.currentTarget.setPointerCapture(event.pointerId)
    scheduleDraw()
  }

  function onPointerMove(event: PointerEvent) {
    const cell = cellAt(event)
    if (mouseCapture) {
      if (mouseCapture.lastEvent.pointerId !== event.pointerId) return
      if (cell) {
        mouseCapture.lastEvent = event
        mouseCapture.cell = cell
      }
    }
    if (selecting === undefined && reportMouse(event, mouseCapture?.button ?? 3, 'move', 1, cell) !== undefined) return
    if (!cell || !frame || selectionAnchor === undefined || event.pointerId !== selecting) return
    const end = cell.row * frame.columns + cell.column
    if (!selection && end === selectionAnchor) return
    selection = { start: selectionAnchor, end }
    scheduleDraw()
  }

  function releaseMouse(event: PointerEvent) {
    if (mouseCapture?.lastEvent.pointerId === event.pointerId) {
      const { lastEvent, cell, button } = mouseCapture
      mouseCapture = undefined
      if (event.type === 'pointerup') reportMouse(event, button, 'release')
      else reportMouse(lastEvent, button, 'release', 1, cell)
    }
    if (selecting === event.pointerId) {
      selecting = undefined
      selectionAnchor = undefined
    }
  }

  function selectWord(event: MouseEvent) {
    if (!frame || (!event.shiftKey && !frame.displayOffset && (frame.modes & MOUSE_REPORTING))) return
    const cell = cellAt(event)
    if (!cell) return
    const { cells, columns } = frame
    let start = cell.row * columns + cell.column
    let end = start
    const category = (text: string) => /[\p{L}\p{N}_]/u.test(text) ? 'word' : text.trim() ? 'punctuation' : 'space'
    const glyphText = (index: number) => {
      const entry = cells[index]
      if (entry?.width !== 0) return entry?.text ?? ''
      const leading = index % columns !== 0 ? cells[index - 1] : undefined
      if (leading?.width === 2) return leading.text
      const following = cells[index + 1]
      return entry.wraps && following?.width === 2 ? following.text : ''
    }
    const group = category(glyphText(start))
    const continuesAfter = (index: number) => (index + 1) % columns !== 0 || cells[index]?.wraps
    while (start > 0 && continuesAfter(start - 1) && category(glyphText(start - 1)) === group) start--
    while (end + 1 < cells.length && continuesAfter(end) && category(glyphText(end + 1)) === group) end++
    selection = { start, end }
    scheduleDraw()
  }

  function viewportText(current: bridge.TerminalFrame): string {
    return readCells(current, 0, current.cells.length - 1).trimEnd()
  }

  function selectedText(): string {
    if (!frame || !selection) return ''
    const [start, end] = selectionRange(frame, selection)
    return readCells(frame, start, end)
  }

  function onCopy(event: ClipboardEvent) {
    const text = selectedText()
    if (!text || !event.clipboardData) return
    event.preventDefault()
    event.clipboardData.setData('text/plain', text)
  }

  function measure(): TerminalSize {
    const padding = container && window.getComputedStyle(container)
    const width = (container?.clientWidth ?? 640) - (padding ? parseFloat(padding.paddingLeft) + parseFloat(padding.paddingRight) : 0)
    const height = (container?.clientHeight ?? 320) - (padding ? parseFloat(padding.paddingTop) + parseFloat(padding.paddingBottom) : 0)
    return {
      columns: Math.max(2, Math.min(512, Math.floor(width / cellWidth))),
      rows: Math.max(1, Math.min(256, Math.floor(height / LINE_HEIGHT))),
    }
  }

  async function applySize() {
    if (props.readOnly) return
    if (!container?.clientWidth || !container.clientHeight) return
    queuedSize = measure()
    if (resizing) return
    resizing = true
    try {
      while (queuedSize) {
        const size = queuedSize
        queuedSize = undefined
        const key = `${size.columns}x${size.rows}`
        if (key === lastSent) continue
        try {
          await bridge.terminalResize(props.terminalId, size.columns, size.rows)
          lastSent = key
          props.onMeasure?.(size)
        } catch (failure) {
          setError(describe(failure))
        }
      }
    } finally {
      resizing = false
    }
  }

  function queueInput(send: () => Promise<void>, bytes = 0, reservesRelease = false): boolean {
    if (props.readOnly) return false
    // A press accepted before the queue fills still needs room for its release.
    const reservedReleases = reportedKeys.size + Number(!!mouseCapture) + Number(reservesRelease)
    if (pendingInputWrites + reservedReleases >= MAX_PENDING_INPUT_WRITES ||
        pendingInputBytes + bytes + reservedReleases * INPUT_RELEASE_BUDGET > MAX_PENDING_INPUT_BYTES) {
      setError('Terminal input is full. Try again after the terminal catches up.')
      return false
    }
    setError(null)
    pendingInputWrites++
    pendingInputBytes += bytes
    inputWrites = inputWrites.then(send).catch((failure) => { setError(describe(failure)) }).finally(() => {
      pendingInputWrites--
      pendingInputBytes -= bytes
    })
    return true
  }

  function write(data: string | number[], followOutput = true, reservesRelease = false): boolean {
    const bytes = typeof data === 'string' ? data.length * 3 : data.length
    return bytes > 0 && queueInput(() => bridge.terminalWrite(props.terminalId, data, followOutput), bytes, reservesRelease)
  }

  function reportFocus(focused: boolean) {
    if (!((frame?.modes ?? 0) & FOCUS_REPORTING)) {
      reportedFocus = undefined
      return
    }
    if (reportedFocus === focused) return
    if (write(focused ? '\x1b[I' : '\x1b[O', false)) reportedFocus = focused
  }

  function updateWindowFocus() {
    reportFocus(props.shown && document.hasFocus() && input === document.activeElement)
  }

  function leaveTerminal() {
    if (mouseCapture) {
      const event = mouseCapture.lastEvent
      releaseMouse(event)
      if (canvas?.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId)
    }
    for (const [code, release] of reportedKeys) {
      reportedKeys.delete(code)
      write(release(), false)
    }
    reportFocus(false)
  }

  function onWindowFocus() {
    if (props.shown && input === document.activeElement) props.onFocus?.(props.terminalId)
    updateWindowFocus()
  }

  function resetSearch() {
    searchGeneration++
    setSearching(false)
    setNoMatch(false)
    searchId = null
    scheduleDraw()
  }

  function closeSearch() {
    resetSearch()
    setFinding(false)
    input?.focus({ preventScroll: true })
  }

  async function find(query: string, direction: bridge.SearchDirection) {
    if (!query || searching()) return
    const generation = ++searchGeneration
    setSearching(true)
    setError(null)
    try {
      if (pendingSearch) await pendingSearch.catch(() => null)
      if (generation !== searchGeneration) return
      pendingSearch = bridge.searchTerminal(props.terminalId, query, searchId, direction)
      const found = await pendingSearch
      if (generation !== searchGeneration) return
      searchId = found
      setNoMatch(found === null)
      scheduleDraw()
    } catch (failure) {
      if (generation === searchGeneration) setError(describe(failure))
    } finally {
      if (generation === searchGeneration) setSearching(false)
    }
  }

  function onKeyDown(event: KeyboardEvent) {
    if (event.defaultPrevented || composing || event.isComposing || event.keyCode === 229) return
    committedComposition = undefined
    if (event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey && frame
      && (event.key === 'PageUp' || event.key === 'PageDown')) {
      event.preventDefault()
      const step = Math.max(1, frame.rows - 1)
      void bridge.terminalScroll(props.terminalId, event.key === 'PageUp' ? step : -step)
        .catch((failure) => setError(describe(failure)))
      return
    }
    const findModifier = mac
      ? event.metaKey && !event.ctrlKey && !event.shiftKey
      : event.ctrlKey && !event.metaKey && event.shiftKey
    if (findModifier && !event.altKey && event.key.toLowerCase() === 'f') {
      event.preventDefault()
      setFinding(true)
      searchInput?.focus()
      return
    }
    if ((event.ctrlKey || event.metaKey) && event.shiftKey && !event.altKey && event.key.toLowerCase() === 'x') {
      event.preventDefault()
      setCopyText(frame ? viewportText(frame) : '')
      copyInput?.focus()
      copyInput?.select()
      return
    }
    if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === 'c') {
      const text = selectedText()
      if (text) {
        event.preventDefault()
        void navigator.clipboard.writeText(text).catch((failure) => setError(describe(failure)))
        return
      }
    }
    const modes = frame?.modes ?? 0
    if (event.metaKey && ['c', 'v'].includes(event.key.toLowerCase())) return
    if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === 'v') return
    // Let the macOS input source insert printable Option characters.
    if (mac && event.altKey && !event.ctrlKey && !event.metaKey && [...event.key].length === 1) return
    const encoded = encodeKittyKey(event, modes)
    const data = encoded?.data ?? encodeLegacyKey(event, modes)
    if (data === null) return
    selection = undefined
    scheduleDraw()
    event.preventDefault()
    const release = event.code && !reportedKeys.has(event.code) ? encoded?.release : undefined
    if (write(data, true, release !== undefined) && release) {
      reportedKeys.set(event.code, release)
    }
  }

  function onKeyUp(event: KeyboardEvent) {
    const release = reportedKeys.get(event.code)
    if (!release) return
    reportedKeys.delete(event.code)
    write(release(event), false)
  }

  function onPaste(event: ClipboardEvent) {
    const text = event.clipboardData?.getData('text/plain')
    if (!text) return
    event.preventDefault()
    if (text.length > MAX_PENDING_INPUT_BYTES) {
      setError('Terminal paste exceeds the 1 MiB input limit.')
      return
    }
    const bracketed = Boolean((frame?.modes ?? 0) & BRACKETED_PASTE)
    const paste = bracketed
      ? `\x1b[200~${text.replaceAll('\x1b', '').replaceAll('\x9b', '')}${BRACKETED_PASTE_END}`
      : text.replace(/\r?\n/g, '\r')
    const bytes = new TextEncoder().encode(paste)
    if (bytes.length > MAX_PENDING_INPUT_BYTES) {
      setError('Terminal paste exceeds the 1 MiB input limit.')
      return
    }
    queueInput(async () => {
      const chunkSize = 16 * 1024
      let offset = 0
      try {
        for (; offset < bytes.length; offset += chunkSize) {
          await bridge.terminalWrite(props.terminalId, Array.from(bytes.subarray(offset, offset + chunkSize)), offset === 0)
        }
      } catch (failure) {
        if (bracketed && offset > 0) {
          try {
            await bridge.terminalWrite(props.terminalId, BRACKETED_PASTE_END, false)
          } catch (cleanupFailure) {
            throw new Error(`${describe(failure)} Could not end the bracketed paste. ${describe(cleanupFailure)}`)
          }
        }
        throw failure
      }
    }, bytes.length)
  }

  onMount(() => {
    const appearance = new MutationObserver(() => {
      paintColors = terminalPaintColors()
      scheduleDraw()
    })
    appearance.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-contrast'] })
    onCleanup(() => appearance.disconnect())
    paintColors = terminalPaintColors()
    const context = canvas?.getContext('2d')
    if (context) {
      context.font = FONT
      cellWidth = Math.max(6, context.measureText('M').width)
      context.fillStyle = paintColors.background
      context.fillRect(0, 0, canvas?.width ?? 0, canvas?.height ?? 0)
    }

    let detached = false
    let unsubscribe: (() => Promise<void>) | undefined
    let mirrorTimer: number | undefined
    void bridge.subscribeTerminal(props.terminalId, (next, changedRows) => {
      if (detached) return
      const previous = frame
      if (previous && ((previous.modes ^ next.modes) & (ALT_SCREEN | KITTY_REPORT_EVENTS))) {
        reportedKeys.clear()
      }
      const selectedRange = selection ?? (selectionAnchor === undefined ? undefined : { start: selectionAnchor, end: selectionAnchor })
      if (previous && selectedRange && (previous.columns !== next.columns || previous.rows !== next.rows || ((previous.modes ^ next.modes) & ALT_SCREEN))) {
        selection = undefined
        selecting = undefined
        selectionAnchor = undefined
        changedRows = null
      } else if (previous && selectedRange) {
        // New output can increase the scrollback offset while leaving the selected cells in place.
        const [first, last] = selectionRange(previous, selectedRange)
        for (let index = first; index <= last; index++) {
          const before = previous.cells[index]
          const after = next.cells[index]
          if (before?.text === after?.text && before?.width === after?.width && before?.wraps === after?.wraps) continue
          selection = undefined
          selecting = undefined
          selectionAnchor = undefined
          changedRows = null
          break
        }
      }
      const oldHighlight = previous?.searchHighlight
      const newHighlight = next.searchHighlight
      if (oldHighlight?.id !== newHighlight?.id || oldHighlight?.start !== newHighlight?.start || oldHighlight?.end !== newHighlight?.end) {
        changedRows = null
      }
      const rows = changedRows && new Set(changedRows)
      if (rows && previous && (previous.cursorRow !== next.cursorRow || previous.cursorColumn !== next.cursorColumn || previous.cursorVisible !== next.cursorVisible || previous.cursorStyle !== next.cursorStyle)) {
        if (previous.cursorVisible) rows.add(previous.cursorRow)
        if (next.cursorVisible) rows.add(next.cursorRow)
      }
      frame = next
      updateWindowFocus()
      scheduleDraw(rows)
      if ((changedRows === null || changedRows.length) && mirrorTimer === undefined) {
        mirrorTimer = window.setTimeout(() => {
          mirrorTimer = undefined
          if (frame && props.shown) setVisibleText(viewportText(frame))
        }, 100)
      }
    }, (failure) => { if (!detached) setError(describe(failure)) }).then((dispose) => {
      if (detached) void dispose().catch(console.error)
      else unsubscribe = dispose
    }).catch((failure) => { if (!detached) setError(describe(failure)) })
    window.addEventListener('focus', onWindowFocus)
    window.addEventListener('blur', leaveTerminal)
    document.addEventListener('visibilitychange', syncViewVisibility)
    onCleanup(() => {
      leaveTerminal()
      window.removeEventListener('focus', onWindowFocus)
      window.removeEventListener('blur', leaveTerminal)
      document.removeEventListener('visibilitychange', syncViewVisibility)
      detached = true
      if (paintRequest !== undefined) window.cancelAnimationFrame(paintRequest)
      window.clearTimeout(mirrorTimer)
      window.clearTimeout(compositionTimer)
      if (input === document.activeElement) props.onFocus?.(null)
      if (unsubscribe) void unsubscribe().catch(console.error)
    })

    function wheel(event: WheelEvent) {
      if (!frame) return
      const reportsHorizontal = !event.shiftKey && !frame.displayOffset && (frame.modes & MOUSE_REPORTING) !== 0
      if (!reportsHorizontal) horizontalWheelRemainder = 0
      if (!event.deltaY && !(event.deltaX && reportsHorizontal)) return
      event.preventDefault()
      const pages = event.deltaMode === WheelEvent.DOM_DELTA_PAGE
      const lines = event.deltaMode === WheelEvent.DOM_DELTA_LINE
      if (reportsHorizontal && event.deltaX) {
        horizontalWheelRemainder += event.deltaX * (pages ? frame.columns : lines ? 1 : 1 / cellWidth)
        const steps = Math.trunc(horizontalWheelRemainder)
        horizontalWheelRemainder -= steps
        if (steps) reportMouse(event, steps < 0 ? 66 : 67, 'press', Math.min(120, Math.abs(steps)))
      }
      if (!event.deltaY) return
      verticalWheelRemainder += event.deltaY * (pages ? frame.rows : lines ? 1 : 1 / LINE_HEIGHT)
      const steps = Math.trunc(verticalWheelRemainder)
      verticalWheelRemainder -= steps
      const count = Math.min(120, Math.abs(steps))
      if (!count) return
      if (reportMouse(event, steps < 0 ? 64 : 65, 'press', count) !== undefined) return
      if (!event.shiftKey && (frame.modes & (ALT_SCREEN | ALTERNATE_SCROLL)) === (ALT_SCREEN | ALTERNATE_SCROLL)) {
        const prefix = frame.modes & APP_CURSOR ? '\x1bO' : '\x1b['
        write((prefix + (steps < 0 ? 'A' : 'B')).repeat(count))
      } else {
        void bridge.terminalScroll(props.terminalId, -Math.sign(steps) * count)
          .catch((failure) => setError(describe(failure)))
      }
    }
    container?.addEventListener('wheel', wheel, { passive: false })
    onCleanup(() => container?.removeEventListener('wheel', wheel))

    let resizeTimer: number | undefined
    const observer = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => void applySize(), 75)
    })
    if (container) observer.observe(container)

    onCleanup(() => {
      observer.disconnect()
      window.clearTimeout(resizeTimer)
      queuedSize = undefined
    })
  })

  return (
    <div class="terminal-view" ref={container} title={`${copyShortcut} opens terminal copy mode`}>
      <canvas
        class="terminal-view__canvas"
        ref={canvas}
        aria-hidden="true"
        onPointerDown={onPointerDown}
        onDblClick={selectWord}
        onPointerMove={onPointerMove}
        onPointerUp={releaseMouse}
        onPointerCancel={releaseMouse}
        onContextMenu={(event) => {
          if (!event.shiftKey && !frame?.displayOffset && ((frame?.modes ?? 0) & MOUSE_REPORTING)) event.preventDefault()
        }}
        onLostPointerCapture={releaseMouse}
      />
      <textarea
        ref={input}
        class="terminal-view__input"
        aria-label="Terminal"
        aria-describedby={instructionsId}
        aria-keyshortcuts={mac ? 'Meta+Shift+X' : 'Control+Shift+X'}
        tabIndex={copyText() === null ? 0 : -1}
        autocapitalize="off"
        autocomplete="off"
        spellcheck={false}
        readOnly={props.readOnly}
        onKeyDown={onKeyDown}
        onKeyUp={onKeyUp}
        onFocus={() => {
          props.onFocus?.(props.terminalId)
          updateWindowFocus()
        }}
        onBlur={() => {
          props.onFocus?.(null)
          leaveTerminal()
        }}
        onPaste={onPaste}
        onCopy={onCopy}
        onCompositionStart={() => {
          composing = true
          committedComposition = undefined
          if (frame?.displayOffset) queueInput(() => bridge.terminalFollowOutput(props.terminalId))
        }}
        onCompositionEnd={(event) => {
          composing = false
          committedComposition = event.data
          window.clearTimeout(compositionTimer)
          compositionTimer = window.setTimeout(() => { committedComposition = undefined }, 0)
          event.currentTarget.value = ''
          write(encodeTerminalText(event.data, frame?.modes ?? 0))
        }}
        onInput={(event) => {
          const inputEvent = event as InputEvent
          if (composing || inputEvent.isComposing) return
          const value = event.currentTarget.value
          event.currentTarget.value = ''
          if (committedComposition !== undefined && (value === committedComposition || inputEvent.data === committedComposition)) {
            committedComposition = undefined
            return
          }
          write(encodeTerminalText(value, frame?.modes ?? 0))
        }}
      />
      <span id={instructionsId} class="terminal-view__instructions">
        Press {copyShortcut} for copy mode. From copy mode, Tab or Shift+Tab moves to another control.
      </span>
      <pre class="terminal-view__text" role="region" aria-label="Terminal output">{visibleText()}</pre>
      <Show when={copyText() !== null}>
        <textarea
          ref={copyInput}
          class="terminal-view__copy"
          aria-label="Copy terminal output"
          aria-description="Use selection shortcuts to copy text. Escape returns to the terminal. Tab or Shift+Tab moves to another control."
          value={copyText() ?? ''}
          readOnly
          onBlur={() => setCopyText(null)}
          onKeyDown={(event) => {
            if (event.key !== 'Escape') return
            event.preventDefault()
            input?.focus({ preventScroll: true })
          }}
        />
      </Show>
      <Show when={finding()}>
        <form class="terminal-view__find" onSubmit={(event) => {
          event.preventDefault()
          const direction = event.submitter?.getAttribute('value') === 'next' ? 'next' : 'previous'
          void find(searchInput?.value ?? '', direction)
        }} onKeyDown={(event) => {
          if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229) return
          event.preventDefault()
          closeSearch()
        }}>
          <input ref={searchInput} type="search" aria-label="Find in terminal" placeholder="Find in terminal" onInput={resetSearch} />
          <button type="submit" value="previous" disabled={searching()}>Find previous</button>
          <button type="submit" value="next" disabled={searching()}>Find next</button>
          <button type="button" onClick={closeSearch}>Close find</button>
          <Show when={noMatch()}><span role="status">No matches</span></Show>
        </form>
      </Show>
    </div>
  )
}

function readCells(frame: bridge.TerminalFrame, start: number, end: number): string {
  let text = ''
  for (let first = start; first <= end;) {
    const rowEnd = first - first % frame.columns + frame.columns
    const after = Math.min(rowEnd, end + 1)
    const part = frame.cells.slice(first, after).map((cell) => cell.text).join('')
    const hardBreak = after === rowEnd && !frame.cells[after - 1]?.wraps
    text += hardBreak ? part.trimEnd() : part
    if (hardBreak && after <= end) text += '\n'
    first = after
  }
  return text
}

function selectionRange(frame: bridge.TerminalFrame, selection: { start: number; end: number }): [number, number] {
  let start = Math.min(selection.start, selection.end)
  let end = Math.max(selection.start, selection.end)
  if (start % frame.columns !== 0 && frame.cells[start - 1]?.width === 2) start--
  if (frame.cells[end]?.width === 2) end++
  return [start, end]
}

function hex(value: number): string {
  return `#${value.toString(16).padStart(6, '0')}`
}

function describe(failure: unknown): string {
  if (typeof failure === 'object' && failure !== null && 'message' in failure) {
    return String(failure.message)
  }
  return String(failure)
}
