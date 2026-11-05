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
  const root = () => {
    const saved = layout()
    const first = visible()[0]
    return saved.revision === 0 && !saved.root && first ? leaf(first.id) : saved.root
  }
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

  function detach(id: string) {
    const node = root()
    if (node) void run(() => save(removePane(node, id)))
  }

  function arrange(axis: bridge.SplitAxis) {
    const ids = paneIds(root())
    if (ids.length > 1) void run(() => save(preset(ids, axis)))
  }

  function resizeSplit(path: SplitPath, ratio: number) {
    const node = root()
    if (node) void run(() => save(updateSplit(node, path, ratio)))
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
        <Show when={paneIds(root()).length > 1}>
          <button type="button" onClick={() => arrange('horizontal')} disabled={pending()}>Side by side</button>
          <button type="button" onClick={() => arrange('vertical')} disabled={pending()}>Stack</button>
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
                <span class="terminal-list__state">{entry.scope === 'project' ? 'shared' : 'task'} · {entry.endedAt === null ? 'live' : 'ended'}{contains(root(), entry.id) ? '' : ' · hidden'}</span>
              </button>
            </li>
          )}
        </For>
        </ul>
      </details>
      <Show when={firstVisible(root(), terminal)} fallback={<p class="empty">Open an instance or start a terminal.</p>}>
        {() => <TerminalTree node={root()!} terminal={terminal} active={active()} select={setSelectedId}
          restart={(id) => void run(async () => { setTerminals(await bridge.restartTerminal(id, 80, 24)) })}
          path={[]} resize={resizeSplit} detach={detach} stop={(id) => void run(() => bridge.closeTerminal(id))} pending={pending()} />}
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

function removePane(node: bridge.TerminalSplit, id: string): bridge.TerminalSplit | null {
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

function firstVisible(node: bridge.TerminalSplit | null, terminal: TerminalLookup): string | null {
  if (!node) return null
  return node.type === 'pane'
    ? (terminal(node.terminalId) ? node.terminalId : null)
    : firstVisible(node.first, terminal) ?? firstVisible(node.second, terminal)
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
  active: string | null
  select: (id: string) => void
  restart: (id: string) => void
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
      <Show when={pane()}>
        {(entry) => (
          <section class="terminal-tile" classList={{ 'terminal-tile--active': entry().id === props.active }}
            aria-label={entry().title} onFocusIn={() => props.select(entry().id)}>
            <div class="terminal-pane__bar">
              <span>{entry().title}</span>
              <button type="button" onClick={() => props.detach(entry().id)} disabled={props.pending}>Hide pane</button>
              <Show when={entry().endedAt === null}>
                <button type="button" onClick={() => props.stop(entry().id)} disabled={props.pending}>Stop process</button>
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
            <div class="terminal-split__child" style={{ flex: preview() ?? branch().ratio }}>
              <TerminalTree {...props} node={branch().first} path={[...props.path, 'first']} />
            </div>
          </Show>
          <Show when={firstVisible(branch().first, props.terminal) && firstVisible(branch().second, props.terminal)}>
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
          <Show when={firstVisible(branch().second, props.terminal)}>
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
const APP_KEYPAD = 1 << 2
const MOUSE_CLICK = 1 << 3
const BRACKETED_PASTE = 1 << 4
const MOUSE_SGR = 1 << 5
const MOUSE_MOTION = 1 << 6
const FOCUS_REPORTING = 1 << 11
const MOUSE_DRAG = 1 << 13
const MOUSE_UTF8 = 1 << 14
const MOUSE_REPORTING = MOUSE_CLICK | MOUSE_MOTION | MOUSE_DRAG

function TerminalCanvas(props: { terminalId: string }) {
  const [error, setError] = createSignal<string | null>(null)
  let input: HTMLTextAreaElement | undefined
  const [finding, setFinding] = createSignal(false)
  const [searching, setSearching] = createSignal(false)
  const [noMatch, setNoMatch] = createSignal(false)
  let match: bridge.TerminalMatch | null = null
  let searchInput: HTMLInputElement | undefined
  let searchGeneration = 0
  let composing = false
  let canvas: HTMLCanvasElement | undefined
  let container: HTMLDivElement | undefined
  let frame: bridge.TerminalFrame | null = null
  let cellWidth = 7.8
  let lastSent = ''
  let inputWrites = Promise.resolve()
  let selection: { start: number; end: number } | undefined
  let selecting: number | undefined
  let mouseButton: number | undefined
  let mousePosition = ''

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
      if (cell.background === DEFAULT_BACKGROUND) return
      const x = (index % current.columns) * cellWidth
      const y = Math.floor(index / current.columns) * LINE_HEIGHT
      context.fillStyle = hex(cell.background)
      context.fillRect(x, y, cellWidth, LINE_HEIGHT)
    })
    if (match) {
      const first = (match.start.line + current.displayOffset) * current.columns + match.start.column
      const last = (match.end.line + current.displayOffset) * current.columns + match.end.column
      context.fillStyle = 'rgba(217, 176, 106, 0.45)'
      for (let index = Math.max(0, first); index <= Math.min(last, current.cells.length - 1); index++) {
        context.fillRect((index % current.columns) * cellWidth, Math.floor(index / current.columns) * LINE_HEIGHT, cellWidth, LINE_HEIGHT)
      }
    }
    if (selection && selection.start !== selection.end) {
      const [start, end] = selectionRange(current, selection)
      context.fillStyle = 'rgba(111, 143, 220, 0.5)'
      for (let index = start; index <= end; index++) {
        context.fillRect((index % current.columns) * cellWidth, Math.floor(index / current.columns) * LINE_HEIGHT, cellWidth, LINE_HEIGHT)
      }
    }
    current.cells.forEach((cell, index) => {
      if (!cell.width || !cell.text || cell.text === ' ') return
      const x = (index % current.columns) * cellWidth
      const y = Math.floor(index / current.columns) * LINE_HEIGHT
      const font = `${cell.italic ? 'italic ' : ''}${cell.bold ? 'bold ' : ''}${FONT}`
      if (context.font !== font) context.font = font
      context.fillStyle = hex(cell.foreground)
      context.fillText(cell.text, x, y + 1, cell.width * cellWidth)
      if (cell.linked) context.fillRect(x, y + LINE_HEIGHT - 2, cell.width * cellWidth, 1)
    })
    if (current.cursorVisible) {
      context.fillStyle = 'rgba(230, 230, 234, 0.35)'
      context.fillRect(current.cursorColumn * cellWidth, current.cursorRow * LINE_HEIGHT, cellWidth, LINE_HEIGHT)
    }
  }

  function cellAt(event: MouseEvent) {
    if (!frame || !canvas) return null
    const bounds = canvas.getBoundingClientRect()
    return {
      column: Math.max(0, Math.min(frame.columns - 1, Math.floor((event.clientX - bounds.left) / cellWidth))),
      row: Math.max(0, Math.min(frame.rows - 1, Math.floor((event.clientY - bounds.top) / LINE_HEIGHT))),
    }
  }

  function reportMouse(event: MouseEvent, button: number, action: 'press' | 'release' | 'move'): boolean {
    const current = frame
    const cell = cellAt(event)
    if (!current || !cell || current.displayOffset || event.shiftKey || !(current.modes & MOUSE_REPORTING)) return false
    const position = `${cell.column},${cell.row}`
    if (action === 'move') {
      if (!(current.modes & MOUSE_MOTION) && (!(current.modes & MOUSE_DRAG) || mouseButton === undefined)) return false
      if (mousePosition === position) return true
      button += 32
    }
    mousePosition = position
    const modifiers = (event.altKey ? 8 : 0) + (event.ctrlKey ? 16 : 0)
    const column = cell.column + 1
    const row = cell.row + 1
    if (current.modes & MOUSE_SGR) {
      write(`\x1b[<${button + modifiers};${column};${row}${action === 'release' ? 'm' : 'M'}`)
    } else {
      const utf8 = (current.modes & MOUSE_UTF8) !== 0
      const limit = utf8 ? 2015 : 223
      if (column <= limit && row <= limit) {
        const code = (action === 'release' ? 3 : button) + modifiers + 32
        const prefix = [27, 91, 77, code]
        const coordinates = utf8
          ? Array.from(new TextEncoder().encode(String.fromCodePoint(column + 32, row + 32)))
          : [column + 32, row + 32]
        write([...prefix, ...coordinates])
      }
    }
    return true
  }

  function onPointerDown(event: PointerEvent & { currentTarget: HTMLCanvasElement }) {
    input?.focus({ preventScroll: true })
    const cell = cellAt(event)
    if (!cell || !frame) return
    if (event.button === 0 && (event.metaKey || event.ctrlKey) && frame.cells[cell.row * frame.columns + cell.column]?.linked) {
      event.preventDefault()
      void bridge.openTerminalLink(props.terminalId, cell.row, cell.column).catch((failure) => setError(describe(failure)))
      return
    }
    const button = [0, 1, 2][event.button]
    if (button !== undefined && reportMouse(event, button, 'press')) {
      event.preventDefault()
      mouseButton = button
      event.currentTarget.setPointerCapture(event.pointerId)
      return
    }
    if (event.button !== 0) return
    event.preventDefault()
    const index = cell.row * frame.columns + cell.column
    selection = { start: index, end: index }
    selecting = event.pointerId
    event.currentTarget.setPointerCapture(event.pointerId)
    draw()
  }

  function onPointerMove(event: PointerEvent) {
    if (selecting === undefined && reportMouse(event, mouseButton ?? 3, 'move')) return
    const cell = cellAt(event)
    if (!cell || !frame || !selection || event.pointerId !== selecting) return
    selection.end = cell.row * frame.columns + cell.column
    draw()
  }

  function releaseMouse(event: PointerEvent) {
    if (mouseButton !== undefined) reportMouse(event, mouseButton, 'release')
    mouseButton = undefined
  }

  function selectedText(): string {
    if (!frame || !selection || selection.start === selection.end) return ''
    const [start, end] = selectionRange(frame, selection)
    const lines = []
    for (let first = start; first <= end;) {
      const after = Math.min(first - first % frame.columns + frame.columns, end + 1)
      const text = frame.cells.slice(first, after).map((cell) => cell.text).join('')
      lines.push(after % frame.columns === 0 ? text.trimEnd() : text)
      first = after
    }
    return lines.join('\n')
  }

  function onCopy(event: ClipboardEvent) {
    const text = selectedText()
    if (!text || !event.clipboardData) return
    event.preventDefault()
    event.clipboardData.setData('text/plain', text)
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
      if (lastSent === key) lastSent = '' 
    }
  }

  function encodeKey(event: KeyboardEvent): string | null {
    if (event.metaKey || event.getModifierState('AltGraph')) return null
    const key = event.key
    if (event.ctrlKey && event.shiftKey && key.toLowerCase() === 'v') return null
    const modifiers = 1 + (event.shiftKey ? 1 : 0) + (event.altKey ? 2 : 0) + (event.ctrlKey ? 4 : 0)
    const modes = frame?.modes ?? 0
    const keypad: Record<string, string> = {
      Numpad0: 'p', Numpad1: 'q', Numpad2: 'r', Numpad3: 's', Numpad4: 't',
      Numpad5: 'u', Numpad6: 'v', Numpad7: 'w', Numpad8: 'x', Numpad9: 'y',
      NumpadMultiply: 'j', NumpadAdd: 'k', NumpadSubtract: 'm', NumpadDecimal: 'n',
      NumpadDivide: 'o', NumpadEnter: 'M',
    }
    if ((modes & APP_KEYPAD) && modifiers === 1 && keypad[event.code]) {
      return `\x1bO${keypad[event.code]}`
    }
    const cursor: Record<string, string> = {
      ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D', Home: 'H', End: 'F',
    }
    const functionKey: Record<string, string> = { F1: 'P', F2: 'Q', F3: 'R', F4: 'S' }
    const final = cursor[key] ?? functionKey[key]
    if (final) {
      if (modifiers !== 1) return `\x1b[1;${modifiers}${final}`
      const prefix = functionKey[key] || (modes & APP_CURSOR) ? 'O' : '['
      return `\x1b${prefix}${final}`
    }
    const numbered: Record<string, number> = {
      Insert: 2, Delete: 3, PageUp: 5, PageDown: 6,
      F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24,
    }
    if (numbered[key]) {
      const modifier = modifiers === 1 ? '' : `;${modifiers}`
      return `\x1b[${numbered[key]}${modifier}~`
    }
    if (key === 'Tab' && event.shiftKey) return '\x1b[Z'
    const controls: Record<string, string> = {
      Enter: '\r', Backspace: '\x7f', Tab: '\t', Escape: '\x1b',
    }
    let data = controls[key]
    if (!data && [...key].length === 1) {
      data = key
      if (event.ctrlKey) {
        const code = key.toUpperCase().charCodeAt(0)
        if (key === ' ' || key === '2') data = '\x00'
        else if (key === '?') data = '\x7f'
        else if (code >= 64 && code <= 95) data = String.fromCharCode(code - 64)
      }
    }
    if (!data) return null
    return event.altKey ? `\x1b${data}` : data
  }

  function write(data: string | number[]) {
    if (!data) return
    setError(null)
    inputWrites = inputWrites
      .then(() => bridge.terminalWrite(props.terminalId, data))
      .catch((failure) => setError(describe(failure)))
  }

  function resetSearch() {
    searchGeneration++
    setSearching(false)
    setNoMatch(false)
    match = null
    draw()
  }

  function closeSearch() {
    resetSearch()
    setFinding(false)
    input?.focus({ preventScroll: true })
  }

  async function find(query: string) {
    if (!query || searching()) return
    const generation = ++searchGeneration
    setSearching(true)
    setError(null)
    try {
      const found = await bridge.searchTerminal(props.terminalId, query, match?.start ?? null)
      if (generation !== searchGeneration) return
      match = found
      setNoMatch(!found)
      draw()
    } catch (failure) {
      if (generation === searchGeneration) setError(describe(failure))
    } finally {
      if (generation === searchGeneration) setSearching(false)
    }
  }

  function onKeyDown(event: KeyboardEvent) {
    if (composing || event.isComposing || event.keyCode === 229) return
    if ((event.metaKey || (event.ctrlKey && event.shiftKey)) && event.key.toLowerCase() === 'f') {
      event.preventDefault()
      setFinding(true)
      searchInput?.focus()
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
    const data = encodeKey(event)
    if (data === null) return
    selection = undefined
    draw()
    event.preventDefault()
    write(data)
  }

  function onPaste(event: ClipboardEvent) {
    const text = event.clipboardData?.getData('text/plain')
    if (!text) return
    event.preventDefault()
    if ((frame?.modes ?? 0) & BRACKETED_PASTE) {
      write(`\x1b[200~${text.replaceAll('\x1b', '').replaceAll('\x9b', '')}\x1b[201~`)
    } else {
      write(text.replace(/\r?\n/g, '\r'))
    }
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
      if (frame && (frame.displayOffset !== next.displayOffset || frame.columns !== next.columns || frame.rows !== next.rows)) {
        selection = undefined
        selecting = undefined
      }
      frame = next
      draw()
    }, (failure) => { if (!detached) setError(describe(failure)) }).then((dispose) => {
      if (detached) void dispose().catch(console.error)
      else unsubscribe = dispose
    }).catch((failure) => { if (!detached) setError(describe(failure)) })
    onCleanup(() => {
      detached = true
      if (unsubscribe) void unsubscribe().catch(console.error)
    })

    function wheel(event: WheelEvent) {
      if (!event.deltaY || !frame) return
      event.preventDefault()
      if (reportMouse(event, event.deltaY < 0 ? 64 : 65, 'press')) return
      const lines = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? event.deltaY : event.deltaY / LINE_HEIGHT
      const count = Math.max(1, Math.round(Math.abs(lines)))
      void bridge.terminalScroll(props.terminalId, -Math.sign(lines) * count).catch(console.error)
    }
    container?.addEventListener('wheel', wheel, { passive: false })
    onCleanup(() => container?.removeEventListener('wheel', wheel))

    let resizeTimer: number | undefined
    const observer = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => void applySize(), 75)
    })
    if (container) observer.observe(container)

    onCleanup(() => { observer.disconnect(); window.clearTimeout(resizeTimer) })
  })

  return (
    <div class="terminal-view" ref={container}>
      <canvas
        class="terminal-view__canvas"
        ref={canvas}
        aria-hidden="true"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={releaseMouse}
        onPointerCancel={releaseMouse}
        onContextMenu={(event) => {
          if (!event.shiftKey && !frame?.displayOffset && ((frame?.modes ?? 0) & MOUSE_REPORTING)) event.preventDefault()
        }}
        onLostPointerCapture={() => { selecting = undefined }}
      />
      <textarea
        ref={input}
        class="terminal-view__input"
        aria-label="Terminal"
        autocapitalize="off"
        autocomplete="off"
        spellcheck={false}
        onKeyDown={onKeyDown}
        onFocus={() => { if ((frame?.modes ?? 0) & FOCUS_REPORTING) write('\x1b[I') }}
        onBlur={() => { if ((frame?.modes ?? 0) & FOCUS_REPORTING) write('\x1b[O') }}
        onPaste={onPaste}
        onCopy={onCopy}
        onCompositionStart={() => { composing = true }}
        onCompositionEnd={(event) => {
          composing = false
          event.currentTarget.value = ''
          write(event.data)
        }}
        onInput={(event) => {
          if (composing || (event as InputEvent).isComposing) return
          write(event.currentTarget.value)
          event.currentTarget.value = ''
        }}
      />
      <Show when={finding()}>
        <form class="terminal-view__find" onSubmit={(event) => {
          event.preventDefault()
          void find(searchInput?.value ?? '')
        }} onKeyDown={(event) => { if (event.key === 'Escape') closeSearch() }}>
          <input ref={searchInput} type="search" aria-label="Find in terminal" placeholder="Find in terminal" onInput={resetSearch} />
          <button type="submit" disabled={searching()}>Find previous</button>
          <button type="button" onClick={closeSearch}>Close find</button>
          <Show when={noMatch()}><span role="status">No matches</span></Show>
        </form>
      </Show>
      <Show when={error()}>{(message) => <p class="error terminal-view__error" role="alert">{message()}</p>}</Show>
    </div>
  )
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
