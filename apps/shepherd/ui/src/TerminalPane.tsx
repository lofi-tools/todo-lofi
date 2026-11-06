/**
 * The Terminals tab. One canvas per live terminal draws the cell grid the engine
 * sends; keys are encoded here and written straight to the PTY. Ended terminals
 * stay listed with their exit code instead of pretending to still be live.
 */

import { For, Show, batch, createEffect, createMemo, createSignal, onCleanup, onMount } from 'solid-js'

import * as bridge from './bridge'
import { KITTY_REPORT_EVENTS, encodeKittyKey, encodeLegacyKey, encodeTerminalText } from './terminal_keys'

interface TerminalSize {
  columns: number
  rows: number
}

const DEFAULT_TERMINAL_SIZE: TerminalSize = { columns: 80, rows: 24 }

export function FreebuffPane(props: { projectId: string; shown: boolean }) {
  const [terminalId, setTerminalId] = createSignal<string>()
  const [error, setError] = createSignal<string>()
  let disposed = false

  function close(id: string) {
    void bridge.closeTerminal(id).catch((failure) => console.warn('Could not close the Freebuff terminal', failure))
  }

  onMount(() => {
    void bridge.startFreebuffTerminal(props.projectId).then((id) => {
      if (disposed) close(id)
      else setTerminalId(id)
    }).catch((failure) => {
      if (!disposed) setError(describe(failure))
    })
  })

  onCleanup(() => {
    disposed = true
    const id = terminalId()
    if (id) close(id)
  })

  return (
    <section class="chat chat--terminal" aria-label="Freebuff terminal" hidden={!props.shown}>
      <Show when={terminalId()} keyed fallback={
        <Show when={error()} fallback={<p class="empty">Opening Freebuff terminal…</p>}>
          {(message) => <p class="error" role="alert">{message()}</p>}
        </Show>
      }>
        {(id) => <TerminalCanvas terminalId={id} shown={props.shown} focusRequest={props.shown ? { id } : undefined} />}
      </Show>
    </section>
  )
}

interface TerminalPaneProps {
  projectId: string
  taskId: string | null
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
}

type TerminalAction = 'newSharedTerminal' | 'newTaskTerminal' | 'splitRight' | 'splitDown' | 'hideTerminal'
  | 'terminalSingle' | 'terminalColumns' | 'terminalStack' | 'terminalQuad'

export interface TerminalCommands {
  available: () => { terminalReady: boolean; terminalPane: boolean; terminalMultiple: boolean; terminalFour: boolean }
  actions: Record<TerminalAction, () => void>
}

export function TerminalPane(props: TerminalPaneProps) {
  const measuredSizes = new Map<string, TerminalSize>()
  const [terminalList, setTerminalList] = createSignal<bridge.TerminalList>()
  const terminals = () => terminalList()?.terminals ?? []
  const [layout, setLayout] = createSignal<bridge.TerminalLayout>({ revision: 0, root: null, inspectorTab: 'terminals' })
  const [layoutLoaded, setLayoutLoaded] = createSignal(false)
  const [selectedId, setSelectedId] = createSignal<string | null>(null)
  const [focusRequest, setFocusRequest] = createSignal<{ id: string }>()
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal<string | null>(null)
  const [hiddenAgentTerminals, setHiddenAgentTerminals] = createSignal(new Set<string>())
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

  const visible = () => terminals().filter((terminal) =>
    terminal.scope === 'project' || terminal.taskId === props.taskId)
  const terminal = (id: string) => visible().find((entry) => entry.id === id)
  const paneVisible = (id: string) => !terminals().some((entry) =>
    entry.id === id && entry.scope === 'task' && entry.taskId !== props.taskId)
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
    setTerminalList((current) => current && current.revision >= snapshot.revision ? current : snapshot)
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

  async function save(root: bridge.TerminalSplit | null) {
    setLayout(await bridge.saveTerminalLayout(props.projectId, { ...layout(), root }))
  }

  async function show(terminalId: string, axis: bridge.SplitAxis = 'vertical') {
    if (!contains(root(), terminalId)) {
      const previous = root()
      const added = leaf(terminalId)
      const target = active()
      const split = (first: bridge.TerminalSplit) => splitInHalf(axis, first, added)
      await save(previous ? (target ? replacePane(previous, target, split) : split(previous)) : added)
    }
    setSelectedId(terminalId)
    setFocusRequest({ id: terminalId })
  }

  function detach(id: string) {
    const node = root()
    if (node) void run(() => save(removePane(node, id)))
  }

  function arrange(axis: bridge.SplitAxis) {
    const ids = attached()
    if (ids.length > 1) void run(() => save(preset(ids, axis)))
  }

  function single() {
    const current = root()
    const id = active()
    if (!current || !id) return
    const next = attachedVisible().filter((pane) => pane !== id)
      .reduce<bridge.TerminalSplit | null>(removePane, current)
    void run(async () => {
      await save(next)
      setFocusRequest({ id })
    })
  }

  function quad() {
    const current = root()
    const ids = attachedVisible()
    const [topLeft, topRight, bottomLeft, bottomRight] = ids
    if (!current || ids.length !== 4) return
    const retained = ids.slice(1).reduce<bridge.TerminalSplit | null>(removePane, current)
    if (!retained) return
    const rows = splitInHalf('vertical',
      splitInHalf('horizontal', leaf(topLeft), leaf(topRight)),
      splitInHalf('horizontal', leaf(bottomLeft), leaf(bottomRight)))
    const id = active()
    void run(async () => {
      await save(replacePane(retained, topLeft, () => rows))
      if (id) setFocusRequest({ id })
    })
  }

  function resizeSplit(path: SplitPath, ratio: number) {
    const node = root()
    if (node) void run(() => save(updateSplit(node, path, ratio)))
  }

  function start(taskId: string | null, axis: bridge.SplitAxis) {
    const activeId = active()
    const size = (activeId && measuredSizes.get(activeId)) || DEFAULT_TERMINAL_SIZE
    const columns = activeId && axis === 'horizontal' ? Math.max(2, Math.floor(size.columns / 2)) : size.columns
    const rows = activeId && axis === 'vertical' ? Math.max(1, Math.floor(size.rows / 2)) : size.rows
    void run(async () => {
      const before = new Set(terminals().map((terminal) => terminal.id))
      const list = await bridge.startTerminal(props.projectId, taskId, columns, rows)
      applyTerminalList(list)
      const created = list.terminals.find((terminal) => !before.has(terminal.id))
      if (created) await show(created.id, axis)
    })
  }

  function startTaskTerminal() {
    const taskId = props.taskId
    if (!taskId || !props.taskHasWorktree || pending()) return
    props.onShow()
    start(taskId, 'vertical')
  }

  createEffect(() => {
    const event = props.listEvent
    if (event) applyTerminalList(event)
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

  function detachActive() {
    const id = active()
    if (id) detach(id)
  }

  function startSharedTerminal() {
    props.onShow()
    start(null, 'vertical')
  }

  onMount(() => {
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
        setLayout(saved)
        props.onLayoutLoaded(saved)
        setLayoutLoaded(true)
      })
    })
    onCleanup(() => { disposed = true })
  })

  return (
    <div class="terminal-pane">
      <div class="terminal-pane__bar">
        <button type="button" onClick={startSharedTerminal} disabled={pending()}>New shared terminal</button>
        <button type="button" onClick={startTaskTerminal} disabled={pending() || !props.taskHasWorktree}
          aria-keyshortcuts={props.taskTerminalShortcut}>New task terminal</button>
        <button type="button" onClick={() => splitActive('horizontal')} disabled={pending() || !active()}>Split right</button>
        <button type="button" onClick={() => splitActive('vertical')} disabled={pending() || !active()}>Split down</button>
        <Show when={attached().length > 1}>
          <Show when={attachedVisible().length > 1}>
            <button type="button" onClick={single} disabled={pending()}>Single</button>
          </Show>
          <button type="button" onClick={() => arrange('horizontal')} disabled={pending()}>Side by side</button>
          <button type="button" onClick={() => arrange('vertical')} disabled={pending()}>Stack</button>
          <Show when={attachedVisible().length === 4}>
            <button type="button" onClick={quad} disabled={pending()}>Quad</button>
          </Show>
        </Show>
      </div>
      <Show when={error()}>{(message) => <p class="error" role="alert">{message()}</p>}</Show>
      <details open>
        <summary>Terminal instances</summary>
        <ul class="terminal-list" aria-label="Terminals">
        <For each={visible()}>
          {(entry) => (
            <li>
              <button type="button" class="terminal-list__item"
                aria-current={entry.id === active() ? 'true' : undefined}
                disabled={pending()} onClick={() => void run(() => show(entry.id))}>
                <span>{entry.title}</span>
                <Show when={props.attention.includes(entry.id)}>
                  <span class="terminal-attention" role="img" aria-label="Terminal needs attention" />
                </Show>
                <span class="terminal-list__state">{entry.scope === 'project' ? 'shared' : 'task'} · {entry.endedAt === null ? 'live' : 'ended'}{contains(root(), entry.id) ? '' : ' · hidden'}</span>
              </button>
            </li>
          )}
        </For>
        <For each={props.agentTerminals}>
          {(entry) => (
            <li>
              <button type="button" class="terminal-list__item" onClick={() => {
                setAgentPaneVisible(entry.id, true)
                if (entry.live) setFocusRequest({ id: entry.id })
              }}>
                <span>agent: {entry.title}</span>
                <span class="terminal-list__state">task · {entry.live ? 'live' : 'ended'}{agentPaneVisible(entry.id) ? '' : ' · hidden'}</span>
              </button>
            </li>
          )}
        </For>
        </ul>
      </details>
      <Show when={props.taskId} keyed>
        <Show when={commands().size > 0}>
          <CommandTerminals commands={commands()} shown={props.shown} />
        </Show>
      </Show>
      <For each={agentTerminalIds()}>
        {(id) => (
          <Show when={agentPaneVisible(id) && agentTerminal(id)}>
            {(entry) => (
              <section class="terminal-tile" data-agent-terminal aria-label={`agent: ${entry().title}`}>
                <div class="terminal-pane__bar">
                  <span>agent: {entry().title}</span>
                  <button type="button" data-terminal-focus={entry().live ? '' : undefined}
                    onClick={() => setAgentPaneVisible(id, false)}>Hide pane</button>
                  <Show when={entry().live}>
                    <button type="button" onClick={() => void run(() => bridge.closeTerminal(id))} disabled={pending()}>Stop process</button>
                  </Show>
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
        <Show when={commands().size === 0 && !agentTerminalIds().some(agentPaneVisible)}><p class="empty">Open an instance or start a terminal.</p></Show>
      }>
        <TerminalTree node={root()!} terminal={terminal} paneVisible={paneVisible} active={active()} shown={props.shown} select={setSelectedId} focusRequest={focusRequest()} onTerminalFocus={props.onTerminalFocus}
          restart={restart} measure={(id, size) => measuredSizes.set(id, size)}
          path={[]} resize={resizeSplit} detach={detach} stop={(id) => void run(() => bridge.closeTerminal(id))} pending={pending()} />
      </Show>
    </div>
  )
}

function CommandTerminals(props: { commands: ReadonlyMap<string, bridge.ToolCallEntry>; shown: boolean }) {
  const [selection, setSelection] = createSignal<string>()
  const ids = createMemo(() => Array.from(props.commands.keys()))
  const selectedId = () => {
    const id = selection()
    return id && props.commands.has(id) ? id : ids().at(-1)
  }

  return (
    <section class="terminal-tile terminal-tile--commands" aria-label="Codex command terminals">
      <div class="terminal-pane__bar">
        <span>Codex commands</span>
        <span class="terminal-list__state">Read-only</span>
      </div>
      <ul class="terminal-list" aria-label="Codex commands">
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
  active: string | null
  shown: boolean
  select: (id: string) => void
  onTerminalFocus: (id: string | null) => void
  focusRequest: { id: string } | undefined
  restart: (id: string) => void
  measure: (id: string, size: TerminalSize) => void
  stop: (id: string) => void
  detach: (id: string) => void
  pending: boolean
}

function TerminalTree(props: TerminalTreeProps) {
  const [preview, setPreview] = createSignal<number | null>(null)
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
                <button type="button" onClick={() => props.detach(id)} disabled={props.pending}>Hide pane</button>
              </div>
              <div class="terminal-ended">
                <p>The saved terminal no longer exists.</p>
              </div>
            </section>
          )}
        </Show>
      }>
        {(entry) => (
          <section class="terminal-tile" classList={{ 'terminal-tile--active': entry().id === props.active }}
            aria-label={entry().title} onFocusIn={() => props.select(entry().id)}>
            <div class="terminal-pane__bar">
              <span>{entry().title}</span>
              <button type="button" data-terminal-focus={entry().endedAt === null ? '' : undefined}
                onClick={() => props.detach(entry().id)} disabled={props.pending}>Hide pane</button>
              <Show when={entry().endedAt === null}>
                <button type="button" onClick={() => props.stop(entry().id)} disabled={props.pending}>Stop process</button>
              </Show>
            </div>
            <Show when={entry().endedAt === null} fallback={
              <div class="terminal-ended">
                <p>{entry().title} ended{formatExit(entry().exitCode)}</p>
                <p><code>{entry().command}</code> in <code>{entry().cwd}</code></p>
                <span class="terminal-list__state">{entry().scope === 'project' ? 'Shared terminal' : 'Task terminal'}</span>
                <button type="button" onClick={() => props.restart(entry().id)} disabled={props.pending}>Restart</button>
              </div>
            }>
              <Show when={entry().id} keyed>
                {(id) => <TerminalCanvas terminalId={id} shown={props.shown} focusRequest={props.focusRequest} onFocus={props.onTerminalFocus} onMeasure={(size) => props.measure(id, size)} />}
              </Show>
            </Show>
          </section>
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
const DEFAULT_BACKGROUND = 0x131316
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

function TerminalCanvas(props: { terminalId: string; shown: boolean; focusRequest: { id: string } | undefined; onFocus?: (id: string | null) => void; onMeasure?: (size: TerminalSize) => void }) {
  const mac = navigator.platform.startsWith('Mac')
  const linux = navigator.platform.startsWith('Linux')
  const [error, setError] = createSignal<string | null>(null)
  let input: HTMLTextAreaElement | undefined
  const [finding, setFinding] = createSignal(false)
  const [copying, setCopying] = createSignal(false)
  const [visibleText, setVisibleText] = createSignal('')
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
      if (frame && !copying()) setVisibleText(viewportText(frame))
    } else {
      if (paintRequest !== undefined) window.cancelAnimationFrame(paintRequest)
      paintRequest = undefined
      dirtyRows = null
    }
  }

  createEffect(() => {
    if (!props.shown) leaveTerminal()
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
      context.fillStyle = hex(DEFAULT_BACKGROUND)
      context.fillRect(0, y, width, LINE_HEIGHT)
      for (let column = 0; column < columns; column++) {
        const cell = current.cells[start + column]
        if (cell.background === DEFAULT_BACKGROUND) continue
        context.fillStyle = hex(cell.background)
        context.fillRect(column * cellWidth, y, cellWidth, LINE_HEIGHT)
      }
      for (const [range, color] of [[highlight, 'rgba(217, 176, 106, 0.45)'], [selected, 'rgba(111, 143, 220, 0.5)']] as const) {
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
        context.fillStyle = 'rgba(230, 230, 234, 0.6)'
        if (current.cursorStyle === 1) context.fillRect(x, y + LINE_HEIGHT - 2, cellWidth, 2)
        else if (current.cursorStyle === 2) context.fillRect(x, y, 2, LINE_HEIGHT)
        else if (current.cursorStyle === 3) {
          context.strokeStyle = context.fillStyle
          context.strokeRect(x + 0.5, y + 0.5, cellWidth - 1, LINE_HEIGHT - 1)
        } else {
          context.fillStyle = 'rgba(230, 230, 234, 0.35)'
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
    if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === 'x' && frame) {
      event.preventDefault()
      setVisibleText(viewportText(frame))
      setCopying(true)
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
    const context = canvas?.getContext('2d')
    if (context) {
      context.font = FONT
      cellWidth = Math.max(6, context.measureText('M').width)
      context.fillStyle = '#131316'
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
          if (frame && props.shown && !copying()) setVisibleText(viewportText(frame))
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
    <div class="terminal-view" ref={container}>
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
        autocapitalize="off"
        autocomplete="off"
        spellcheck={false}
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
      <pre class="terminal-view__text" aria-label="Terminal output">{visibleText()}</pre>
      <Show when={copying()}>
        <textarea
          ref={copyInput}
          class="terminal-view__copy"
          aria-label="Copy terminal output"
          aria-description="Use the selection shortcuts to copy text. Escape returns to the terminal."
          value={visibleText()}
          readOnly
          onKeyDown={(event) => {
            if (event.key !== 'Escape') return
            event.preventDefault()
            setCopying(false)
            if (frame) setVisibleText(viewportText(frame))
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
      <Show when={error()}>{(message) => <p class="error terminal-view__error" role="alert">{message()}</p>}</Show>
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
