/**
 * The Terminals tab. One canvas per live terminal draws the cell grid the engine
 * sends; keys are encoded here and written straight to the PTY. Ended terminals
 * stay listed with their exit code instead of pretending to still be live.
 */

import { For, Show, createEffect, createSignal, onCleanup, onMount } from 'solid-js'

import * as bridge from './bridge'

interface TerminalPaneProps {
  projectId: string
  taskId: string | null
  taskHasWorktree: boolean
}

export function TerminalPane(props: TerminalPaneProps) {
  const [terminals, setTerminals] = createSignal<bridge.TerminalSnapshot[]>([])
  const [layout, setLayout] = createSignal<bridge.TerminalLayout>({ revision: 0, root: null })
  const [selectedId, setSelectedId] = createSignal<string | null>(null)
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal<string | null>(null)

  const visible = () => terminals().filter((terminal) =>
    terminal.scope === 'project' || terminal.taskId === props.taskId)
  const terminal = (id: string) => visible().find((entry) => entry.id === id)
  const root = () => layout().root ?? (visible()[0] ? leaf(visible()[0].id) : null)
  const active = () => {
    const id = selectedId()
    const node = root()
    return id && terminal(id) && contains(node, id) ? id : firstVisible(node, terminal)
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
      const split = (first: bridge.TerminalSplit): bridge.TerminalSplit =>
        ({ type: 'split', axis, ratio: 500, first, second: added })
      await save(previous ? (target ? replacePane(previous, target, split) : split(previous)) : added)
    }
    setSelectedId(terminalId)
  }

  function start(taskId: string | null, axis: bridge.SplitAxis) {
    void run(async () => {
      const before = new Set(terminals().map((terminal) => terminal.id))
      const list = await bridge.startTerminal(props.projectId, taskId, 80, 24)
      setTerminals(list)
      const created = list.find((terminal) => !before.has(terminal.id))
      if (created) await show(created.id, axis)
    })
  }

  onMount(() => {
    let disposed = false
    const load = async () => {
      const [list, saved] = await Promise.all([bridge.terminalList(props.projectId), bridge.terminalLayout(props.projectId)])
      if (disposed) return
      setTerminals(list)
      if (saved) setLayout(saved)
    }
    void run(load)
    void bridge.subscribeToEvents((event) => {
      if (!disposed && event.type === 'terminalListChanged' && event.data.projectId === props.projectId) {
        setTerminals(event.data.terminals)
      }
    }).catch((failure) => setError(describe(failure)))
    onCleanup(() => { disposed = true })
  })

  return (
    <div class="terminal-pane">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => start(null, 'vertical')} disabled={pending()}>New shared terminal</button>
        <button type="button" onClick={() => start(props.taskId, 'vertical')} disabled={pending() || !props.taskHasWorktree}>New task terminal</button>
        <button type="button" onClick={() => start(props.taskId && props.taskHasWorktree ? props.taskId : null, 'horizontal')} disabled={pending() || !active()}>Split right</button>
        <button type="button" onClick={() => start(props.taskId && props.taskHasWorktree ? props.taskId : null, 'vertical')} disabled={pending() || !active()}>Split down</button>
      </div>
      <Show when={error()}>{(message) => <p class="error" role="alert">{message()}</p>}</Show>
      <ul class="terminal-list" aria-label="Terminals">
        <For each={visible()}>
          {(entry) => (
            <li>
              <button type="button" class="terminal-list__item"
                aria-current={entry.id === active() ? 'true' : undefined}
                disabled={pending()} onClick={() => void run(() => show(entry.id))}>
                <span>{entry.title}</span>
                <span class="terminal-list__state">{entry.scope === 'project' ? 'shared' : 'task'} · {entry.endedAt === null ? 'live' : 'ended'}</span>
              </button>
            </li>
          )}
        </For>
      </ul>
      <Show when={root()} fallback={<p class="empty">No terminals yet. Start one in this project.</p>}>
        {(node) => <TerminalTree node={node()} terminal={terminal} active={active()} select={setSelectedId}
          restart={(id) => void run(async () => { setTerminals(await bridge.restartTerminal(id, 80, 24)) })}
          stop={(id) => void run(() => bridge.closeTerminal(id))} pending={pending()} />}
      </Show>
    </div>
  )
}

function leaf(terminalId: string): bridge.TerminalSplit {
  return { type: 'pane', terminalId }
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

type TerminalLookup = (id: string) => bridge.TerminalSnapshot | undefined

function firstVisible(node: bridge.TerminalSplit | null, terminal: TerminalLookup): string | null {
  if (!node) return null
  return node.type === 'pane'
    ? (terminal(node.terminalId) ? node.terminalId : null)
    : firstVisible(node.first, terminal) ?? firstVisible(node.second, terminal)
}

interface TerminalTreeProps {
  node: bridge.TerminalSplit
  terminal: TerminalLookup
  active: string | null
  select: (id: string) => void
  restart: (id: string) => void
  stop: (id: string) => void
  pending: boolean
}

function TerminalTree(props: TerminalTreeProps) {
  const split = () => props.node.type === 'split' ? props.node : null
  const pane = () => props.node.type === 'pane' ? props.terminal(props.node.terminalId) : undefined
  return (
    <Show when={split()} fallback={
      <Show when={pane()}>
        {(entry) => (
          <section class="terminal-tile" classList={{ 'terminal-tile--active': entry().id === props.active }}
            aria-label={entry().title} onFocusIn={() => props.select(entry().id)}>
            <div class="terminal-pane__bar">
              <span>{entry().title}</span>
              <Show when={entry().endedAt === null}>
                <button type="button" onClick={() => props.stop(entry().id)} disabled={props.pending}>Stop</button>
              </Show>
            </div>
            <Show when={entry().endedAt === null} fallback={
              <div class="terminal-ended">
                <p class="empty">{entry().title} ended{formatExit(entry().exitCode)} in {entry().cwd}</p>
                <button type="button" onClick={() => props.restart(entry().id)} disabled={props.pending}>Restart</button>
              </div>
            }>
              <Show when={entry().id} keyed>
                {(id) => <TerminalCanvas terminalId={id} />}
              </Show>
            </Show>
          </section>
        )}
      </Show>
    }>
      {(branch) => (
        <div class={`terminal-split terminal-split--${branch().axis}`}>
          <Show when={firstVisible(branch().first, props.terminal)}>
            <div class="terminal-split__child" style={{ flex: branch().ratio }}>
              <TerminalTree {...props} node={branch().first} />
            </div>
          </Show>
          <Show when={firstVisible(branch().second, props.terminal)}>
            <div class="terminal-split__child" style={{ flex: 1000 - branch().ratio }}>
              <TerminalTree {...props} node={branch().second} />
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

function TerminalCanvas(props: { terminalId: string }) {
  let canvas: HTMLCanvasElement | undefined
  let container: HTMLDivElement | undefined
  let frame: bridge.TerminalFrame | null = null
  let cellWidth = 7.8
  let lastSent = ''

  function draw() {
    if (!canvas || !frame) return
    const context = canvas.getContext('2d')
    if (!context) return

    const ratio = window.devicePixelRatio || 1
    const width = Math.ceil(frame.columns * cellWidth)
    const height = frame.rows * LINE_HEIGHT
    canvas.width = Math.ceil(width * ratio)
    canvas.height = Math.ceil(height * ratio)
    canvas.style.width = `${width}px`
    canvas.style.height = `${height}px`
    context.setTransform(ratio, 0, 0, ratio, 0, 0)
    context.fillStyle = '#131316'
    context.fillRect(0, 0, width, height)
    context.font = FONT
    context.textBaseline = 'top'

    const current = frame
    current.cells.forEach((cell, index) => {
      const column = index % current.columns
      const row = Math.floor(index / current.columns)
      const x = column * cellWidth
      const y = row * LINE_HEIGHT

      if (cell.background !== DEFAULT_BACKGROUND) {
        context.fillStyle = hex(cell.background)
        context.fillRect(x, y, cellWidth, LINE_HEIGHT)
      }
      if (cell.text !== ' ') {
        context.fillStyle = hex(cell.foreground)
        context.fillText(cell.text, x, y + 1)
      }
      if (row === current.cursorRow && column === current.cursorColumn) {
        context.fillStyle = 'rgba(230, 230, 234, 0.35)'
        context.fillRect(x, y, cellWidth, LINE_HEIGHT)
      }
    })
  }

  function measure() {
    const width = container?.clientWidth ?? 640
    const height = container?.clientHeight ?? 320
    return {
      columns: Math.max(20, Math.floor(width / cellWidth)),
      rows: Math.max(5, Math.floor(height / LINE_HEIGHT)),
    }
  }

  async function applySize() {
    if (!container?.clientWidth || !container.clientHeight) return
    const size = measure()
    const key = `${size.columns}x${size.rows}`
    if (key === lastSent) return
    lastSent = key
    try {
      await bridge.terminalResize(props.terminalId, size.columns, size.rows)
    } catch {
      // A terminal that already ended cannot be resized; the ended view replaces
      // this canvas when the next terminal-list event arrives.
    }
  }

  function encodeKey(event: KeyboardEvent): string | null {
    if (event.metaKey) return null
    const key = event.key
    if (key.length === 1) {
      if (event.ctrlKey) {
        const code = key.toUpperCase().charCodeAt(0)
        if (code >= 64 && code <= 95) return String.fromCharCode(code - 64)
        if (key === ' ') return '\u0000'
      }
      return event.altKey ? `\u001b${key}` : key
    }
    const sequences: Record<string, string> = {
      Enter: '\r',
      Backspace: '\u007f',
      Tab: '\t',
      Escape: '\u001b',
      ArrowUp: '\u001b[A',
      ArrowDown: '\u001b[B',
      ArrowRight: '\u001b[C',
      ArrowLeft: '\u001b[D',
      Home: '\u001b[H',
      End: '\u001b[F',
      PageUp: '\u001b[5~',
      PageDown: '\u001b[6~',
      Delete: '\u001b[3~',
    }
    return sequences[key] ?? null
  }

  function onKeyDown(event: KeyboardEvent) {
    const data = encodeKey(event)
    if (data === null) return
    event.preventDefault()
    void bridge.terminalWrite(props.terminalId, data).catch(() => undefined)
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
    void bridge.subscribeTerminal(props.terminalId, (next) => {
      if (detached) return
      frame = next
      draw()
    }).then((dispose) => {
      if (detached) void dispose().catch(console.error)
      else unsubscribe = dispose
    }).catch(console.error)
    onCleanup(() => {
      detached = true
      if (unsubscribe) void unsubscribe().catch(console.error)
    })

    const observer = new ResizeObserver(() => void applySize())
    if (container) observer.observe(container)

    onCleanup(() => observer.disconnect())
  })

  return (
    <div class="terminal-view" ref={container}>
      <canvas
        class="terminal-view__canvas"
        ref={canvas}
        tabindex={0}
        aria-label="Terminal"
        onKeyDown={onKeyDown}
      />
    </div>
  )
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
