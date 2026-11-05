import * as bridge from '../src/bridge'

declare const __SHEPHERD_SMOKE_PROJECT__: string

export function button(label: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll('button')).find((entry) => entry.textContent?.trim() === label)
}

export async function waitFor<T>(label: string, read: () => T | null | undefined | false): Promise<T> {
  const until = performance.now() + 12_000
  while (performance.now() < until) {
    const value = read()
    if (value) return value
    await new Promise((resolve) => window.setTimeout(resolve, 25))
  }
  throw new Error(`Timed out waiting for ${label}`)
}

function terminalOutput(scope: ParentNode = document): string {
  return scope.querySelector('.terminal-view__text')?.textContent ?? ''
}

function terminalPane(title: string): HTMLElement | undefined {
  return Array.from(document.querySelectorAll<HTMLElement>('.terminal-tile'))
    .find((entry) => entry.getAttribute('aria-label') === title)
}

async function measureBurst(terminalId: string): Promise<number> {
  const original = CanvasRenderingContext2D.prototype.fillText
  let tail = ''
  let paintedAt = 0
  CanvasRenderingContext2D.prototype.fillText = function (text, x, y, maxWidth) {
    original.call(this, text, x, y, maxWidth)
    if (!paintedAt && this.canvas.classList.contains('terminal-view__canvas')) {
      tail = (tail + text).slice(-6)
      if (tail === 'S4_END') paintedAt = performance.now()
    }
  }
  try {
    const started = performance.now()
    await bridge.terminalWrite(terminalId, "awk 'BEGIN {for(i=0;i<16384;i++) printf \"%062d\\n\", i; printf \"S4_%s\\n\", \"END\"}'\r")
    await waitFor('1 MiB burst tail painted', () => paintedAt)
    return Math.round((paintedAt - started) * 10) / 10
  } finally {
    CanvasRenderingContext2D.prototype.fillText = original
  }
}

async function measurePacing(terminalId: string) {
  const original = CanvasRenderingContext2D.prototype.fillText
  const seen = new Map<string, number>()
  let tail = ''
  CanvasRenderingContext2D.prototype.fillText = function (text, x, y, maxWidth) {
    original.call(this, text, x, y, maxWidth)
    if (!this.canvas.classList.contains('terminal-view__canvas')) return
    tail = (tail + text).slice(-6)
    if (/^S4_T\d{2}$/.test(tail) && !seen.has(tail)) seen.set(tail, performance.now())
  }
  try {
    await bridge.terminalWrite(terminalId, "i=0; while [ \"$i\" -lt 24 ]; do printf 'S4_T%02d\\n' \"$i\"; i=$((i+1)); sleep 0.05; done\r")
    await waitFor('all 24 paced output markers to paint', () => seen.size === 24)
    const times = [...seen.values()]
    let maxGap = 0
    for (let index = 1; index < times.length; index++) {
      maxGap = Math.max(maxGap, times[index] - times[index - 1])
    }
    return { markers: seen.size, maxGapMs: Math.round(maxGap * 10) / 10 }
  } finally {
    CanvasRenderingContext2D.prototype.fillText = original
  }
}

function press(input: HTMLElement, key: string) {
  input.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }))
}

async function exerciseAlternateScreen(terminalId: string, input: HTMLTextAreaElement) {
  await bridge.terminalWrite(terminalId, "vim -Nu NONE -n -i NONE -c 'call setline(1, \"S4_\" . \"VIM\")' -c 'set nomodified'\r")
  await waitFor('Vim alternate screen', () => terminalOutput().includes('S4_VIM') && !terminalOutput().includes('S4_READY'))
  for (const key of ['Escape', ':', 'q', 'Enter']) press(input, key)
  await waitFor('main screen restored after Vim', () => terminalOutput().includes('S4_READY') && !terminalOutput().includes('S4_VIM'))

  await bridge.terminalWrite(terminalId, "printf 'S4_%s\\n' LESS | LESS= less -R\r")
  await waitFor('less alternate screen', () => terminalOutput().includes('S4_LESS') && !terminalOutput().includes('S4_READY'))
  press(input, 'q')
  await waitFor('main screen restored after less', () => terminalOutput().includes('S4_READY') && !terminalOutput().includes('S4_LESS'))
}

async function exerciseComposition(terminalId: string, input: HTMLTextAreaElement) {
  await bridge.terminalWrite(terminalId, "IFS= read -r composed; printf '\\nS4_%s:%s\\n' IME \"$composed\"\r")
  input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }))
  input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: 'かな' }))
  input.value = 'かな'
  input.dispatchEvent(new InputEvent('input', { bubbles: true, data: 'かな', inputType: 'insertText' }))
  press(input, 'Enter')
  await waitFor('composed text delivered once', () => terminalOutput().split('\n').some((line) => line.trim() === 'S4_IME:かな'))
}

async function exerciseKitty(terminalId: string, input: HTMLTextAreaElement) {
  const program = String.raw`
import os, select, termios, tty

def expect(expected):
    data = b''
    while len(data) < len(expected) and select.select([0], [], [], 5)[0]:
        chunk = os.read(0, len(expected) - len(data))
        if not chunk:
            break
        data += chunk
    assert data == expected, repr(data)

settings = termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1, b'\x1b[>3u\x1b[?u')
    expect(b'\x1b[?3u')
    os.write(1, b'\r\nS4_' + b'KITTY_READY\r\n')
    expect(b'\x1b[27u\x1b[27;1:3u')
    os.write(1, b'\r\nS4_' + b'KITTY_PASS\r\n')
finally:
    os.write(1, b'\x1b[<u')
    termios.tcsetattr(0, termios.TCSANOW, settings)
`
  const argument = "'" + program.replaceAll("'", "'\"'\"'") + "'"
  await bridge.terminalWrite(terminalId, `python3 -c ${argument}\r`)
  await waitFor('Kitty negotiation reply in the PTY', () => terminalOutput().includes('S4_KITTY_READY'))
  for (const type of ['keydown', 'keyup']) {
    input.dispatchEvent(new KeyboardEvent(type, {
      key: 'Escape', code: 'Escape', bubbles: true, cancelable: true,
    }))
  }
  await waitFor('Kitty key press and release in the PTY', () => terminalOutput().includes('S4_KITTY_PASS'))
}

async function exerciseSplit(projectId: string, terminalId: string, title: string) {
  const split = await waitFor('split action', () => {
    const entry = button('Split right')
    return entry && !entry.disabled && entry
  })
  split.click()
  const divider = await waitFor('live split divider', () => document.querySelector<HTMLElement>('.terminal-divider'))
  await waitFor('both canvases in the split', () => document.querySelectorAll('.terminal-view__canvas').length === 2)
  await waitFor('split layout save', () => !button('Split right')?.disabled)
  press(divider, 'ArrowRight')
  await waitFor('persisted resize', () => divider.getAttribute('aria-valuenow') === '55')

  await bridge.terminalWrite(terminalId, "printf 'S4_%s:%s\\n' VIEW \"$$\"\r")
  const identity = await waitFor('first PTY identity', () => {
    const pane = terminalPane(title)
    return pane && terminalOutput(pane).match(/S4_VIEW:\d+/)?.[0]
  })
  const first = await waitFor('first pane', () => terminalPane(title))
  const hide = Array.from(first.querySelectorAll('button')).find((entry) => entry.textContent === 'Hide pane')
  if (!hide) throw new Error('The first pane has no Hide action')
  hide.click()
  await waitFor('first pane detached', () => !terminalPane(title))
  const reopen = await waitFor('hidden terminal instance', () => Array.from(document.querySelectorAll<HTMLButtonElement>('.terminal-list__item'))
    .find((entry) => !entry.disabled && entry.querySelector('span')?.textContent === title))
  reopen.click()
  await waitFor('same PTY screen after reopening', () => {
    const pane = terminalPane(title)
    return pane && terminalOutput(pane).includes(identity)
  })
  const terminals = await bridge.terminalList(projectId)
  if (terminals.length !== 2 || terminals.some((entry) => entry.endedAt !== null)) {
    throw new Error('Hiding and reopening changed the live terminal records')
  }
}

async function smoke() {
  await waitFor('shell startup', () => {
    const add = button('Add project')
    return add && !add.disabled
  })
  const projects = await bridge.addProject(__SHEPHERD_SMOKE_PROJECT__)
  const project = projects.projects.find((entry) => entry.path === __SHEPHERD_SMOKE_PROJECT__)
  if (!project) throw new Error('The fixture project was not registered')
  const projectButton = await waitFor('fixture project', () => Array.from(document.querySelectorAll<HTMLButtonElement>('.project'))
    .find((entry) => entry.querySelector('.project__name')?.textContent === project.name))
  projectButton.click()
  const start = await waitFor('shared terminal action', () => {
    const entry = button('New shared terminal')
    return entry && !entry.disabled && entry
  })
  start.click()
  const input = await waitFor('terminal input', () => document.querySelector<HTMLTextAreaElement>('.terminal-view__input'))
  await waitFor('terminal canvas frame', () => document.querySelector<HTMLCanvasElement>('.terminal-view__canvas')?.style.height)
  const [terminal] = await bridge.terminalList(project.id)
  if (!terminal) throw new Error('The fixture terminal was not started')
  input.focus()
  await bridge.terminalWrite(terminal.id, "printf '\nS4_%s\n' READY\r")
  await waitFor('PTY output in the webview', () => terminalOutput().includes('S4_READY'))
  await exerciseAlternateScreen(terminal.id, input)
  await exerciseComposition(terminal.id, input)
  await exerciseKitty(terminal.id, input)
  const burstPaintMs = await measureBurst(terminal.id)
  const pacing = await measurePacing(terminal.id)
  await exerciseSplit(project.id, terminal.id, terminal.title)
  return { ok: true, roundTrip: true, splitReattach: true, pacing, alternateScreen: true, compositionEvents: true, kittyRoundTrip: true, burstPaintMs }
}

void (async () => {
  let result
  try {
    result = await smoke()
  } catch (error) {
    const message = typeof error === 'object' && error !== null && 'message' in error
      ? String(error.message)
      : String(error)
    result = { ok: false, error: message }
  }
  try {
    await fetch('/__shepherd-terminal-smoke', { method: 'POST', body: JSON.stringify(result) })
  } finally {
    button('Quit')?.click()
  }
})()
