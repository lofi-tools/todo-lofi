/**
 * The Terminals tab. One canvas per live terminal draws the cell grid the engine
 * sends; keys are encoded here and written straight to the PTY. Ended terminals
 * stay listed with their exit code instead of pretending to still be live.
 */

import { For, Show, createEffect, createSignal, onCleanup, onMount } from 'solid-js'

import * as bridge from './bridge'

interface TerminalPaneProps {
  projectId: string
}

export function TerminalPane(props: TerminalPaneProps) {
  const [terminals, setTerminals] = createSignal<bridge.TerminalSnapshot[]>([])
  const [selectedId, setSelectedId] = createSignal<string | null>(null)
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal<string | null>(null)

  const selected = () => terminals().find((terminal) => terminal.id === selectedId()) ?? null

  function applyList(list: bridge.TerminalSnapshot[]) {
    setTerminals(list)
    const current = selectedId()
    if (current && list.some((terminal) => terminal.id === current)) return
    const live = list.find((terminal) => terminal.endedAt === null)
    setSelectedId(live?.id ?? list[list.length - 1]?.id ?? null)
  }

  async function refresh(projectId: string) {
    applyList(await bridge.terminalList(projectId))
  }

  async function start() {
    setPending(true)
    setError(null)
    try {
      const list = await bridge.startTerminal(props.projectId, 80, 24)
      const created = list[list.length - 1]
      if (created) setSelectedId(created.id)
      setTerminals(list)
    } catch (failure) {
      setError(describe(failure))
    } finally {
      setPending(false)
    }
  }

  async function close(terminalId: string) {
    setPending(true)
    setError(null)
    try {
      await bridge.closeTerminal(terminalId)
      await refresh(props.projectId)
    } catch (failure) {
      setError(describe(failure))
    } finally {
      setPending(false)
    }
  }

  createEffect(() => {
    const projectId = props.projectId
    setSelectedId(null)
    void refresh(projectId).catch((failure) => setError(describe(failure)))
  })

  onMount(() => {
    void bridge
      .subscribeToEvents((event) => {
        if (event.type === 'terminalListChanged' && event.data.projectId === props.projectId) {
          applyList(event.data.terminals)
        }
      })
      .catch((failure) => setError(describe(failure)))
  })

  return (
    <div class="terminal-pane">
      <div class="terminal-pane__bar">
        <button type="button" onClick={() => void start()} disabled={pending()}>
          New terminal
        </button>
        <Show when={selected()}>
          {(terminal) => (
            <button
              type="button"
              onClick={() => void close(terminal().id)}
              disabled={pending() || terminal().endedAt !== null}
            >
              Close
            </button>
          )}
        </Show>
        <Show when={error()}>{(message) => <span class="error">{message()}</span>}</Show>
      </div>

      <Show
        when={selected()}
        fallback={<p class="empty">No terminals yet. Start one in this project.</p>}
      >
        {(terminal) => (
          <div class="terminal-pane__body">
            <ul class="terminal-list" aria-label="Terminals">
              <For each={terminals()}>
                {(entry) => (
                  <li>
                    <button
                      type="button"
                      class="terminal-list__item"
                      aria-current={entry.id === selectedId() ? 'true' : undefined}
                      onClick={() => setSelectedId(entry.id)}
                    >
                      <span>{entry.title}</span>
                      <span class="terminal-list__state">
                        {entry.endedAt === null ? 'live' : 'ended'}
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>

            <div class="terminal-pane__view">
              <Show
                when={terminal().endedAt === null}
                fallback={
                  <div class="terminal-ended">
                    <p class="empty">
                      {terminal().title} ended{formatExit(terminal().exitCode)} in{' '}
                      {terminal().cwd}
                    </p>
                    <button type="button" onClick={() => void start()} disabled={pending()}>
                      Restart
                    </button>
                  </div>
                }
              >
                <TerminalCanvas terminalId={terminal().id} />
              </Show>
            </div>
          </div>
        )}
      </Show>
    </div>
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

    void bridge
      .terminalFrame(props.terminalId)
      .then((initial) => {
        if (!initial) return
        frame = initial
        draw()
        void applySize()
      })
      .catch(() => undefined)

    void bridge
      .subscribeTerminal(props.terminalId, (next) => {
        frame = next
        draw()
      })
      .catch(() => undefined)

    const observer = new ResizeObserver(() => void applySize())
    if (container) observer.observe(container)
    canvas?.focus()
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
