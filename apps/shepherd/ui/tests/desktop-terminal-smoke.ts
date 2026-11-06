import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'

import * as bridge from '../src/bridge'

declare const __SHEPHERD_SMOKE_FISH__: boolean
declare const __SHEPHERD_SMOKE_PROJECT__: string
declare const __SHEPHERD_SMOKE_PHASE__: string
declare const __SHEPHERD_SMOKE_BACKGROUND__: boolean

const INTERRUPTED_TASK = 'Other terminal scope'
const WORKTREE_PROMPT = 'Prepare the disposable terminal worktree'
const WORKSPACE_FILE = 'workspace-guide.md'
const NETWORK_PROMPT = "Run this shell command and report only the status code: curl -s -o /dev/null -w '%{http_code}' https://example.com"
const CODEX_WAIT = 75_000
const windowChecks = !__SHEPHERD_SMOKE_BACKGROUND__

export function button(label: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll('button'))
    .find((entry) => (entry.getAttribute('aria-label') ?? entry.textContent?.trim()) === label)
}

export async function waitFor<T>(
  label: string,
  read: () => T | null | undefined | false | Promise<T | null | undefined | false>,
  timeout = 12_000,
): Promise<T> {
  const until = performance.now() + timeout
  while (performance.now() < until) {
    const result = read()
    const value = result instanceof Promise ? await result : result
    if (value) return value
    await new Promise((resolve) => window.setTimeout(resolve, 25))
  }
  const visibility = windowChecks && document.hidden ? ' The webview is hidden. Keep the smoke window visible.' : ''
  throw new Error(`Timed out waiting for ${label}.${visibility}`)
}

function enabledButton(label: string) {
  return waitFor(label, () => {
    const entry = button(label)
    return entry && !entry.disabled && entry
  })
}

function chatState() {
  return document.querySelector('.chat__state')?.textContent
}

async function sendPrompt(text: string) {
  const input = await waitFor('active composer', () => {
    const textarea = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Prompt"]')
    return textarea && !textarea.disabled && textarea
  })
  input.value = text
  input.dispatchEvent(new InputEvent('input', { bubbles: true }))
  const send = await enabledButton('Send')
  send.click()
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

async function exerciseAccessibility(input: HTMLTextAreaElement) {
  const tab = await waitFor('selected Terminals tab', () =>
    document.querySelector<HTMLElement>('#inspector-tab-terminals[aria-selected="true"]'))
  const panelId = tab.getAttribute('aria-controls')
  const panel = panelId ? document.getElementById(panelId) : null
  const header = tab.closest('.shell__header-inspector')?.getBoundingClientRect()
  const windowHeader = document.querySelector('.shell__header')?.getBoundingClientRect()
  const pane = panel?.closest('.inspector__pane')?.getBoundingClientRect()
  if (!header || !windowHeader || !pane || header.top !== windowHeader.top ||
    header.height !== windowHeader.height || header.x !== pane.x || header.width !== pane.width || header.bottom !== pane.top ||
    panel?.getAttribute('aria-labelledby') !== tab.id) {
    throw new Error('The Terminals tab does not align with its labelled inspector panel')
  }

  input.focus()
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'X', ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true }))
  const copy = await waitFor('terminal copy mode', () =>
    document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Copy terminal output"]'))
  if (document.activeElement !== copy || !copy.value.includes('S4_READY') ||
    copy.selectionStart !== 0 || copy.selectionEnd !== copy.value.length) {
    throw new Error('Copy mode did not focus and select the terminal viewport')
  }
  press(copy, 'Escape')
  await waitFor('copy mode closes and restores focus', () => !copy.isConnected && document.activeElement === input)
}

async function pressPaneShortcut(title: string, key: string, shiftKey: boolean) {
  const input = await waitFor(`input in ${title}`, () =>
    terminalPane(title)?.querySelector<HTMLTextAreaElement>('.terminal-view__input'))
  const mac = navigator.platform.startsWith('Mac')
  input.focus()
  input.dispatchEvent(new KeyboardEvent('keydown', {
    key, shiftKey, metaKey: mac, ctrlKey: !mac, bubbles: true, cancelable: true,
  }))
}

async function typeIntoTerminal(terminal: bridge.TerminalSnapshot, command: string) {
  const input = await waitFor(`terminal input in ${terminal.title}`, () => {
    const field = terminalPane(terminal.title)?.querySelector<HTMLTextAreaElement>('.terminal-view__input')
    return field && (!windowChecks || document.activeElement === field) && field
  })
  if (!windowChecks) {
    await bridge.terminalWrite(terminal.id, command + '\r')
    return
  }
  for (const key of command) press(input, key)
  press(input, 'Enter')
}

async function exerciseShellInterrupt(terminalId: string, input: HTMLTextAreaElement) {
  const script = 'import time; print("S1_" + "WAIT"); time.sleep(60)'
  await bridge.terminalWrite(terminalId, `python3 -u -c ${shellQuote(script)}\r`)
  await waitFor('foreground command in the terminal', () => terminalOutput().includes('S1_WAIT'))
  input.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'c', code: 'KeyC', ctrlKey: true, bubbles: true, cancelable: true,
  }))
  await waitFor('Ctrl+C interrupts the foreground command', () => terminalOutput().includes('KeyboardInterrupt'))
}

async function exerciseUnclaimedControlKey(terminalId: string, input: HTMLTextAreaElement) {
  const mac = navigator.platform.startsWith('Mac')
  const byte = mac ? '05' : '17'
  await runPtyProbe(terminalId, String.raw`
os.write(1, b'\r\nS4_' + b'CONTROL_READY\r\n')
expect(bytes.fromhex('${byte}'))
os.write(1, b'\r\nS4_' + b'CONTROL_PASS\r\n')
`, '')
  await waitFor('raw PTY input before checking control keys', () => terminalOutput().includes('S4_CONTROL_READY'))
  input.dispatchEvent(new KeyboardEvent('keydown', {
    key: mac ? 'E' : 'w', shiftKey: mac, ctrlKey: true, bubbles: true, cancelable: true,
  }))
  await waitFor('unclaimed control key received by the PTY', () => terminalOutput().includes('S4_CONTROL_PASS'))
}

async function exerciseAlternateScreen(terminalId: string, input: HTMLTextAreaElement) {
  await bridge.terminalWrite(terminalId, "vim -Nu NONE -n -i NONE -c 'call setline(1, \"S4_\" . \"VIM\")' .shepherd-terminal-vim\r")
  await waitFor('Vim alternate screen', () => terminalOutput().includes('S4_VIM') && !terminalOutput().includes('S4_READY'))
  for (const key of ['A', ...'_EDIT', 'Escape']) press(input, key)
  await waitFor('Vim buffer reflects keyboard input', () => terminalOutput().includes('S4_VIM_EDIT'))
  for (const key of [':', 'w', 'q', 'Enter']) press(input, key)
  await waitFor('main screen restored after Vim', () => terminalOutput().includes('S4_READY') && !terminalOutput().includes('S4_VIM'))
  await bridge.terminalWrite(terminalId, 'cat .shepherd-terminal-vim && rm .shepherd-terminal-vim\r')
  await waitFor('Vim edit saved to disk', () => terminalOutput().split('\n').some((line) => line.trim() === 'S4_VIM_EDIT'))

  await bridge.terminalWrite(terminalId, "awk 'BEGIN {for (line=1; line<=120; line++) printf \"S4_%s%03d\\n\", \"LESS\", line}' | LESS= less -R\r")
  await waitFor('less alternate screen', () => terminalOutput().includes('S4_LESS001') && !terminalOutput().includes('S4_READY'))
  press(input, 'PageDown')
  await waitFor('less paged forward', () => !terminalOutput().includes('S4_LESS001') && /S4_LESS\d{3}/.test(terminalOutput()))
  press(input, 'G')
  await waitFor('less end of output', () => terminalOutput().includes('S4_LESS120'))
  press(input, 'q')
  await waitFor('main screen restored after less', () => terminalOutput().includes('S4_VIM_EDIT') && !/S4_LESS\d{3}/.test(terminalOutput()))
}

async function exerciseFuzzyFinder(terminalId: string, input: HTMLTextAreaElement) {
  const command = "selection=$(printf '%s.%s\\n' first txt second txt third md | FZF_DEFAULT_OPTS= FZF_DEFAULT_OPTS_FILE= fzf --layout=reverse --no-sort); printf 'S4_%s:%s\\n' FZF \"$selection\""
  await bridge.terminalWrite(terminalId, command + '\r')
  await waitFor('fzf alternate screen', () => terminalOutput().includes('first.txt')
    && terminalOutput().includes('third.md') && !terminalOutput().includes('S4_VIM_EDIT'))
  for (const key of 'txt') press(input, key)
  await waitFor('fzf filtered to text files', () => terminalOutput().includes('first.txt')
    && terminalOutput().includes('second.txt') && !terminalOutput().includes('third.md'))
  press(input, 'ArrowDown')
  press(input, 'Enter')
  await waitFor('fzf selection returned to the shell', () =>
    terminalOutput().split('\n').some((line) => line.trim() === 'S4_FZF:second.txt'))
}

async function exerciseWideCopy(terminalId: string, input: HTMLTextAreaElement) {
  await bridge.terminalWrite(terminalId, "printf '\\033[?1049h\\033[H%s\\n' '界!'; IFS= read -r answer; printf '\\033[?1049lS4_%s\\n' COPY_DONE\r")
  await waitFor('wide glyph and punctuation on the terminal screen', () =>
    terminalOutput().split('\n')[0].trim() === '界!')
  const canvas = await waitFor('terminal canvas for copy', () =>
    document.querySelector<HTMLCanvasElement>('.terminal-view__canvas'))
  const context = canvas.getContext('2d')
  if (!context) throw new Error('The terminal canvas has no 2D context')
  const cellWidth = context.measureText('M').width
  const { left, top } = canvas.getBoundingClientRect()
  for (const [column, expected] of [[2, '!'], [0, '界'], [1, '界']] as const) {
    canvas.dispatchEvent(new MouseEvent('dblclick', {
      bubbles: true, button: 0, clientX: left + (column + 0.5) * cellWidth, clientY: top + 1,
    }))
    const clipboard = new DataTransfer()
    input.dispatchEvent(new ClipboardEvent('copy', { bubbles: true, cancelable: true, clipboardData: clipboard }))
    const text = clipboard.getData('text/plain')
    if (text !== expected) throw new Error(`Copy at column ${column} returned ${JSON.stringify(text)} instead of ${expected}`)
  }
  press(input, 'Enter')
  await waitFor('main screen restored after wide selection', () => terminalOutput().includes('S4_COPY_DONE'))
}

async function exerciseComposition(terminalId: string, input: HTMLTextAreaElement) {
  const promptRow = 6
  await bridge.terminalWrite(terminalId, `index=0; while [ "$index" -lt 100 ]; do printf 'history %s\\n' "$index"; index=$((index + 1)); done; printf '\\033[${promptRow};8HS4_%s> ' IME_READY; IFS= read -r composed; printf '\\nS4_%s:%s\\n' IME "$composed"\r`)
  const ready = () => terminalOutput().includes('S4_IME_READY')
  const promptTop = 8 + (promptRow - 1) * 17
  await waitFor('composition prompt at a displaced cursor', () => ready() &&
    parseFloat(input.style.top) === promptTop && parseFloat(input.style.left) > 8)
  const { top, left } = input.style
  await bridge.terminalScroll(terminalId, 40)
  await waitFor('scrollback away from the composition prompt', () => !ready() && input.style.top !== top)
  input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }))
  await waitFor('composition returns the input anchor to the live cursor', () =>
    ready() && input.style.top === top && input.style.left === left)
  input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: 'かな' }))
  input.value = 'かな'
  input.dispatchEvent(new InputEvent('input', { bubbles: true, data: 'かな', inputType: 'insertText' }))
  press(input, 'Enter')
  await waitFor('composed text delivered once', () => terminalOutput().split('\n').some((line) => line.trim() === 'S4_IME:かな'))
}

async function reportIme(details: object) {
  const response = await fetch('/__shepherd-terminal-smoke', {
    method: 'POST', body: JSON.stringify(details),
  })
  if (!response.ok) throw new Error('Could not report the native IME probe')
}

async function exerciseNativeIme(
  commitTimeout: number,
  expected?: readonly { text: string; event: 'compositionend' | 'input' }[],
) {
  const project = await openFixtureProject()
  const start = await enabledButton('New shared terminal')
  start.click()
  const input = await waitFor('terminal input', () => document.querySelector<HTMLTextAreaElement>('.terminal-view__input'))
  const { terminals: [terminal] } = await bridge.terminalList(project.id)
  if (!terminal) throw new Error('The fixture terminal was not started')
  await waitFor('initial terminal frame', () => document.querySelector<HTMLCanvasElement>('.terminal-view__canvas')?.style.height)
  await ptyColumns(terminal.id, terminal.title, 'NATIVE_IME_SIZED')

  const received: string[] = []
  const positions = [[5, 3], [10, 8], [6, 4]].slice(0, expected?.length ?? 2)
  for (const [index, [row, column]] of positions.entries()) {
    const attempt = index + 1
    const expectedInput = expected?.[index]
    const eventName = expectedInput?.event ?? 'compositionend'
    const commits: string[] = []
    const onCommit = (event: Event) => {
      if (event.isTrusted && (event instanceof CompositionEvent || event instanceof InputEvent) && event.data) {
        commits.push(event.data)
      }
    }
    input.addEventListener(eventName, onCommit)
    try {
      await bridge.terminalWrite(terminal.id, `printf '\\033[2J\\033[${row};${column}HIME%s> ' ${attempt}; IFS= read -r composed; printf '\\nS4_NATIVE_IME_RESULT_%s:%s\\n' ${attempt} "$composed"\r`)
      await waitFor(`native IME prompt ${attempt}`, () => terminalOutput().includes(`IME${attempt}>`))
      input.focus({ preventScroll: true })
      if (expected) {
        await reportIme({ ime: 'ready', attempt })
      } else {
        await waitFor(`native IME caret ${attempt}`, () => parseFloat(input.style.top) === 8 + (row - 1) * 17)
        const { left, top } = input.getBoundingClientRect()
        await reportIme({ ime: 'ready', attempt, cursor: [left, top], devicePixelRatio })
      }
      const selection = await waitFor('native IME commit', () => commits[0], commitTimeout)
      await reportIme({ ime: 'committed', attempt, selection })
      const marker = `S4_NATIVE_IME_RESULT_${attempt}:`
      const text = await waitFor('native text reaches the PTY', () =>
        terminalOutput().split('\n').map((line) => line.trim())
          .find((line) => line.startsWith(marker))?.slice(marker.length))
      const validText = expectedInput ? text === expectedInput.text : /\p{Script=Han}/u.test(text)
      if (commits.length !== 1 || selection !== text || !validText) {
        throw new Error(`Expected one ${expectedInput?.text ?? 'Pinyin'} commit in the shell, got ${JSON.stringify({ commits, text })}`)
      }
      received.push(text)
    } finally {
      input.removeEventListener(eventName, onCommit)
    }
  }
  if (received[0] === received[1]) throw new Error('Both prompts received the same character')
  return { ok: true, nativeIme: true, received }
}

async function exerciseMacosVisibility() {
  const project = await openFixtureProject()
  const start = await enabledButton('New shared terminal')
  start.click()
  const context = await waitFor('painted terminal canvas', () => {
    const canvas = document.querySelector<HTMLCanvasElement>('.terminal-view__canvas')
    if (canvas?.style.height) return canvas.getContext('2d')
  })
  const { terminals: [terminal] } = await bridge.terminalList(project.id)
  if (!terminal) throw new Error('The fixture terminal was not started')

  const appWindow = getCurrentWindow()
  const originalFillText = context.fillText
  let hiddenPaints = 0
  let markerPainted = false
  context.fillText = function (...args) {
    if (document.hidden) hiddenPaints++
    if (args[0] === 'Ω') markerPainted = true
    originalFillText.apply(this, args)
  }
  try {
    await appWindow.minimize()
    try {
      await waitFor('minimized window to hide the webview', async () => document.hidden && await appWindow.isMinimized())
      await bridge.terminalWrite(terminal.id, "printf '\\316\\251\\n'\r")
      await waitFor('PTY output received while minimized', () =>
        document.hidden && terminalOutput().includes('Ω'))
      if (markerPainted || hiddenPaints) throw new Error('The terminal painted while minimized')
    } finally {
      await appWindow.unminimize()
    }
    await waitFor('restored window to paint the received output', async () =>
      !document.hidden && markerPainted && !(await appWindow.isMinimized()))
    return { ok: true, macosVisibility: true, hiddenPaints }
  } finally {
    context.fillText = originalFillText
  }
}

function shellQuote(value: string): string {
  return "'" + value.replaceAll("'", "'\"'\"'") + "'"
}

function runPtyProbe(terminalId: string, steps: string, reset: string) {
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
    exec(${JSON.stringify(steps)})
finally:
    os.write(1, bytes.fromhex(${JSON.stringify(reset)}))
    termios.tcsetattr(0, termios.TCSANOW, settings)
`
  return bridge.terminalWrite(terminalId, `python3 -c ${shellQuote(program)}\r`)
}

async function exerciseKitty(terminalId: string, input: HTMLTextAreaElement) {
  // Separate shell commands fit within macOS's canonical PTY input limit.
  await runPtyProbe(terminalId, String.raw`
os.write(1, b'\x1b[>3u\x1b[?2026h\x1b[?1049h\x1b[=0u\x1b[?u\x1b[>1u\x1b[?u'
            b'\x1b[<u\x1b[?1049l\x1b[?u\x1b[?2026l')
expect(b'\x1b[?0u\x1b[?1u\x1b[?3u')
os.write(1, b'\r\nS4_' + b'KITTY_SCREEN_PASS\r\n')
`, '1b5b3f323032366c1b5b3f313034396c1b5b3c75')
  await waitFor('independent Kitty modes across synchronized screen switches', () =>
    terminalOutput().includes('S4_KITTY_SCREEN_PASS'))

  await runPtyProbe(terminalId, String.raw`
os.write(1, b'\x1b[>3u\x1b[?u')
expect(b'\x1b[?3u')
os.write(1, b'\r\nS4_' + b'KITTY_READY\r\n')
expect(b'\x1b[97;5u\x1b[97;1:3u\r\t\x7f'
       b'\x1b[13;3u\x1b[13;3:3u\x1b[9;6u\x1b[9;6:3u\x1b[127;5u\x1b[127;5:3u'
       b'\x1b[27u\x1b[27;1:3u')
os.write(1, b'\r\nS4_' + b'KITTY_PASS\r\n')
`, '1b5b3c75')
  await waitFor('Kitty negotiation reply in the PTY', () => terminalOutput().includes('S4_KITTY_READY'))
  input.focus()
  for (const type of ['keydown', 'keyup']) {
    input.dispatchEvent(new KeyboardEvent(type, {
      key: 'a', code: 'KeyA', ctrlKey: type === 'keydown', bubbles: true, cancelable: true,
    }))
  }
  for (const key of [
    { key: 'Enter' }, { key: 'Tab' }, { key: 'Backspace' },
    { key: 'Enter', altKey: true },
    { key: 'Tab', ctrlKey: true, shiftKey: true },
    { key: 'Backspace', ctrlKey: true },
  ]) {
    for (const type of ['keydown', 'keyup']) {
      input.dispatchEvent(new KeyboardEvent(type, { code: key.key, ...key, bubbles: true, cancelable: true }))
    }
  }
  input.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'Escape', code: 'Escape', bubbles: true, cancelable: true,
  }))
  input.blur()
  await waitFor('Kitty key press and release in the PTY', () => terminalOutput().includes('S4_KITTY_PASS'))
  input.focus()
}

async function exerciseFish(terminalId: string, input: HTMLTextAreaElement) {
  const setup = [
    'bind ctrl-i ' + shellQuote("printf 'S4_%s\\n' FISH_CTRL"),
    'bind tab ' + shellQuote("printf 'S4_%s\\n' FISH_TAB"),
    "function fish_prompt; printf 'S4_%s ' FISH_READY; end",
  ].join('; ')
  await bridge.terminalWrite(terminalId, 'fish --no-config -i -C ' + shellQuote(setup) + "; printf 'S4_%s\\n' FISH_EXIT\r")
  await waitFor('interactive fish prompt', () => terminalOutput().includes('S4_FISH_READY'))
  for (const key of [{ key: 'i', code: 'KeyI', ctrlKey: true }, { key: 'Tab', code: 'Tab' }]) {
    for (const type of ['keydown', 'keyup']) {
      input.dispatchEvent(new KeyboardEvent(type, { ...key, bubbles: true, cancelable: true }))
    }
  }
  await waitFor('distinct Ctrl+I and Tab bindings in fish', () =>
    terminalOutput().includes('S4_FISH_CTRL') && terminalOutput().includes('S4_FISH_TAB'))
  await bridge.terminalWrite(terminalId, 'exit\r')
  await waitFor('return from fish to the fixture shell', () => terminalOutput().includes('S4_FISH_EXIT'))
}

async function exerciseBracketedPaste(terminalId: string, input: HTMLTextAreaElement) {
  await runPtyProbe(terminalId, String.raw`
os.write(1, b'\x1b[?2004h\r\nS4_' + b'PASTE_READY\r\n')
expect(b'\x1b[200~a' + 'é'.encode() * 9000 + b'\nend\x1b[201~')
os.write(1, b'\r\nS4_' + b'PASTE_PASS\r\n')
`, '1b5b3f323030346c')
  await waitFor('PTY bracketed paste mode', () => terminalOutput().includes('S4_PASTE_READY'))
  const clipboard = new DataTransfer()
  clipboard.setData('text/plain', 'a' + 'é'.repeat(9000) + '\n\x1b\x9bend')
  input.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: clipboard }))
  await waitFor('chunked UTF-8 bracketed paste in the PTY', () => terminalOutput().includes('S4_PASTE_PASS'))
}

async function exerciseMouse(terminalId: string) {
  await runPtyProbe(terminalId, String.raw`
os.write(1, b'\x1b[?1000h\x1b[?1006h\r\nS4_' + b'MOUSE_READY\r\n')
expect(b'\x1b[<66;1;1M\x1b[<67;1;1M\x1b[<65;1;1M')
os.write(1, b'\r\nS4_' + b'MOUSE_PASS\r\n')
`, '1b5b3f313030306c1b5b3f313030366c')
  await waitFor('PTY SGR mouse mode', () => terminalOutput().includes('S4_MOUSE_READY'))
  const canvas = await waitFor('terminal mouse surface', () => document.querySelector('.terminal-view__canvas'))
  const { left, top } = canvas.getBoundingClientRect()
  for (const delta of [{ deltaX: -1 }, { deltaX: 1 }, { deltaY: 1 }]) {
    canvas.dispatchEvent(new WheelEvent('wheel', {
      ...delta, bubbles: true, cancelable: true, deltaMode: WheelEvent.DOM_DELTA_LINE,
      clientX: left + 1, clientY: top + 1,
    }))
  }
  await waitFor('SGR wheel reports in the PTY', () => terminalOutput().includes('S4_MOUSE_PASS'))
}

async function exerciseHyperlink(terminalId: string) {
  const program = String.raw`import os; os.write(1, b'\x1b[H\x1b]8;;https://example.invalid/shepherd-terminal\x1b\\S4_' + b'LINK\x1b]8;;\x1b\\\x1b[6;1H')`
  await bridge.terminalWrite(terminalId, `python3 -c ${shellQuote(program)}\r`)
  await waitFor('OSC 8 hyperlink in terminal output', () => terminalOutput().startsWith('S4_LINK'))
  const canvas = await waitFor('terminal link surface', () => document.querySelector('.terminal-view__canvas'))
  await new Promise(requestAnimationFrame)
  const { left, top } = canvas.getBoundingClientRect()
  const mac = navigator.platform.startsWith('Mac')
  canvas.dispatchEvent(new PointerEvent('pointerdown', {
    bubbles: true, cancelable: true, button: 0, metaKey: mac, ctrlKey: !mac,
    clientX: left + 1, clientY: top + 1,
  }))
  const url = await waitFor('the terminal hyperlink to reach the desktop opener', async () => {
    const error = canvas.closest('.terminal-view')?.querySelector('.terminal-view__error')?.textContent
    if (error) throw new Error(`Opening the terminal hyperlink failed: ${error}`)
    const response = await fetch('/__shepherd-terminal-smoke/opener')
    if (response.status === 404) return false
    if (!response.ok) throw new Error(`Reading the smoke opener log failed with HTTP ${response.status}`)
    return response.text()
  })
  if (url !== 'https://example.invalid/shepherd-terminal\n') {
    throw new Error(`The terminal opener received ${JSON.stringify(url)}`)
  }
}

function paneWidth(title: string): number {
  const canvas = terminalPane(title)?.querySelector('canvas')
  return parseFloat(canvas?.style.width ?? '') || 0
}

async function ptyColumns(terminalId: string, title: string, marker: string): Promise<number> {
  const program = "import os, sys; size = os.get_terminal_size(0); print(f'S4_{sys.argv[1]}:{size.lines} {size.columns}')"
  await bridge.terminalWrite(terminalId, `python3 -c ${shellQuote(program)} ${shellQuote(marker)}\r`)
  const size = new RegExp('S4_' + marker + ':(\\d+) (\\d+)')
  const match = await waitFor(marker + ' PTY dimensions', () => {
    const pane = terminalPane(title)
    return pane && terminalOutput(pane).match(size)
  })
  return Number(match[2])
}

async function exerciseInspectorResize(terminal: bridge.TerminalSnapshot) {
  const divider = await waitFor('inspector resize control', () =>
    document.querySelector<HTMLElement>('[aria-label="Resize inspector"]'))
  function fittedColumns() {
    const canvas = terminalPane(terminal.title)?.querySelector('canvas')
    const view = canvas?.parentElement
    const context = canvas?.getContext('2d')
    if (!canvas || !view || !context) return
    const padding = getComputedStyle(view)
    const available = view.clientWidth - parseFloat(padding.paddingLeft) - parseFloat(padding.paddingRight)
    const cellWidth = Math.max(6, context.measureText('M').width)
    const columns = Math.floor(available / cellWidth)
    if (columns >= 2 && parseFloat(canvas.style.width) === Math.ceil(columns * cellWidth)) return columns
  }
  const initialFit = await waitFor('terminal frame fitted to the inspector', fittedColumns)
  const initialColumns = await ptyColumns(terminal.id, terminal.title, 'INSPECTOR_BEFORE')
  if (initialColumns !== initialFit) throw new Error('The PTY and initial inspector frame disagree')
  const inspectorWidth = Number(divider.getAttribute('aria-valuenow'))
  press(divider, 'ArrowLeft')
  await waitFor('wider inspector', () => Number(divider.getAttribute('aria-valuenow')) > inspectorWidth)
  const expandedFit = await waitFor('terminal frame fitted to the wider inspector', fittedColumns)
  const expandedColumns = await ptyColumns(terminal.id, terminal.title, 'INSPECTOR_AFTER')
  if (expandedColumns !== expandedFit || expandedColumns <= initialColumns) {
    throw new Error('The PTY did not fit the wider inspector')
  }
  press(divider, 'ArrowRight')
  await waitFor('terminal frame fitted to the restored inspector', () =>
    Number(divider.getAttribute('aria-valuenow')) === inspectorWidth && fittedColumns() === initialFit)
  const restoredColumns = await ptyColumns(terminal.id, terminal.title, 'INSPECTOR_RESTORED')
  if (restoredColumns !== initialColumns) throw new Error('The PTY did not restore its original width')
}

async function exerciseSplit(projectId: string, terminalId: string, title: string) {
  const initialWidth = windowChecks ? await waitFor('painted terminal before splitting', () => paneWidth(title)) : 0
  const initialColumns = windowChecks ? await ptyColumns(terminalId, title, 'INITIAL_SIZE') : 0
  const split = await enabledButton('Split right')
  if (windowChecks) await pressPaneShortcut(title, 'D', true)
  else split.click()
  await waitFor('both canvases in the split', () => document.querySelectorAll('.terminal-view__canvas').length === 2)
  await waitFor('split layout save', () => !button('Split right')?.disabled)
  if (windowChecks) {
    const divider = await waitFor('live split divider', () => document.querySelector<HTMLElement>('.terminal-divider'))
    const splitWidth = await waitFor('narrower PTY frame after split', () => {
      const width = paneWidth(title)
      return width > 0 && width < initialWidth && width
    })
    const splitColumns = await ptyColumns(terminalId, title, 'SPLIT_SIZE')
    press(divider, 'ArrowRight')
    await waitFor('persisted resize', () => divider.getAttribute('aria-valuenow') === '55')
    await waitFor('wider PTY frame after resize', () => paneWidth(title) > splitWidth)
    const resizedColumns = await ptyColumns(terminalId, title, 'RESIZED_SIZE')
    if (!(splitColumns < resizedColumns && resizedColumns < initialColumns)) {
      throw new Error('The shell should see the terminal columns change when splitting and resizing')
    }
  }

  await bridge.terminalWrite(terminalId, "printf 'S4_%s:%s\\n' VIEW \"$$\"\r")
  const identity = await waitFor('first PTY identity', () => {
    const pane = terminalPane(title)
    return pane && terminalOutput(pane).match(/S4_VIEW:\d+/)?.[0]
  })
  if (windowChecks) {
    const mac = navigator.platform.startsWith('Mac')
    await pressPaneShortcut(title, mac ? 'w' : 'W', !mac)
  } else {
    const hide = await waitFor('hide action in the shared terminal', () =>
      Array.from(terminalPane(title)?.querySelectorAll('button') ?? [])
        .find((entry) => entry.textContent === 'Hide pane' && !entry.disabled))
    hide.click()
  }
  await waitFor('first pane detached', () => !terminalPane(title))
  const reopen = await waitFor('hidden terminal instance', () => Array.from(document.querySelectorAll<HTMLButtonElement>('.terminal-list__item'))
    .find((entry) => !entry.disabled && entry.querySelector('span')?.textContent === title))
  reopen.click()
  await waitFor('same PTY screen after reopening', () => {
    const pane = terminalPane(title)
    return pane && terminalOutput(pane).includes(identity)
  })
  const { terminals } = await bridge.terminalList(projectId)
  if (terminals.length !== 2 || terminals.some((entry) => entry.endedAt !== null)) {
    throw new Error('Hiding and reopening changed the live terminal records')
  }
}

async function exerciseSearch(terminal: bridge.TerminalSnapshot, input: HTMLTextAreaElement) {
  const query = 'S4_SEARCH[.]'
  const command = "printf '\\n'; for match in first second; do printf 'S4_%s[.]:%s\\n' SEARCH \"$match\"; awk 'BEGIN {for (line=1; line<=80; line++) print line}'; done; printf 'S4_%s\\n' SEARCH_END"
  await bridge.terminalWrite(terminal.id, command + '\r')
  await waitFor('search markers have moved into scrollback', () =>
    terminalOutput().includes('S4_SEARCH_END') && !terminalOutput().includes(query))

  const mac = navigator.platform.startsWith('Mac')
  input.focus()
  input.dispatchEvent(new KeyboardEvent('keydown', {
    key: mac ? 'f' : 'F', metaKey: mac, ctrlKey: !mac, shiftKey: !mac, bubbles: true, cancelable: true,
  }))
  const search = await waitFor('focused terminal find field', () => {
    const field = document.querySelector<HTMLInputElement>('input[aria-label="Find in terminal"]')
    return field && document.activeElement === field && field
  })
  search.value = query
  search.dispatchEvent(new InputEvent('input', { bubbles: true }))
  const reveal = (result: string) => waitFor(`search reveals ${result}`, () =>
    terminalOutput().split('\n').some((line) => line.trim() === `${query}:${result}`))
  const previous = await enabledButton('Find previous')
  previous.click()
  await reveal('second')
  const canvas = terminalPane(terminal.title)?.querySelector('canvas')
  const context = canvas?.getContext('2d')
  if (!context) throw new Error('The terminal canvas has no 2D context')
  await waitFor('the canvas highlights the search match', () => {
    const [red, green, blue] = context.getImageData(devicePixelRatio, 16 * devicePixelRatio, 1, 1).data
    return red > green && green > blue
  })
  let frame: bridge.TerminalFrame | undefined
  let frameError: unknown
  const dispose = await bridge.subscribeTerminal(terminal.id, (next) => { frame = next }, (failure) => { frameError = failure })
  try {
    const pinned = await waitFor('the highlighted scrollback frame', () => frame?.searchHighlight && frame)
    await bridge.terminalWrite(terminal.id, "awk 'BEGIN {for (line=1; line<=100; line++) print line}'\r", false)
    await waitFor('the pinned highlight to survive new PTY output', () => {
      if (frameError) throw frameError
      return frame && frame.displayOffset >= pinned.displayOffset + 100
        && frame.searchHighlight?.id === pinned.searchHighlight?.id
        && frame.searchHighlight?.start === pinned.searchHighlight?.start
    })
  } finally {
    await dispose()
  }
  await waitFor('previous search completed', () => !previous.disabled)
  previous.click()
  await reveal('first')
  const next = await enabledButton('Find next')
  next.click()
  await reveal('second')
  search.focus()
  press(search, 'Escape')
  await waitFor('find closes and returns terminal focus', () =>
    !search.isConnected && document.activeElement === input)
  await typeIntoTerminal(terminal, "printf 'S4_%s\\n' SEARCH_RETURN")
  await waitFor('typing after search shows live shell output', () =>
    terminalOutput().split('\n').some((line) => line.trim() === 'S4_SEARCH_RETURN'))
}

async function exerciseProjectSwitch(project: bridge.ProjectSnapshot, terminal: bridge.TerminalSnapshot) {
  await bridge.terminalWrite(terminal.id, "project_marker=preserved; printf 'S4_%s\\n' PROJECT_BEFORE\r")
  await waitFor('shared terminal output before project switch', () =>
    terminalOutput().split('\n').some((line) => line.trim() === 'S4_PROJECT_BEFORE'))

  const secondProject = 'second-project'
  await bridge.addProject(`${project.path}/../${secondProject}`)
  const secondRow = await enabledButton(secondProject)
  secondRow.click()
  await waitFor('other project without the shared terminal', () =>
    secondRow.getAttribute('aria-current') === 'true' && !terminalPane(terminal.title))
  await openInspectorTab('review')
  await bridge.terminalWrite(terminal.id, "printf 'S4_%s:%s\\n' PROJECT_AFTER \"$project_marker\"\r")

  const originalRow = await enabledButton(project.name)
  originalRow.click()
  await waitFor('same shell after switching projects', () => {
    const pane = terminalPane(terminal.title)
    return pane && terminalOutput(pane).includes('S4_PROJECT_BEFORE')
      && terminalOutput(pane).includes('S4_PROJECT_AFTER:preserved')
  })
  secondRow.click()
  await waitFor('Review still selected for the other project', () =>
    secondRow.getAttribute('aria-current') === 'true'
      && document.querySelector('#inspector-tab-review[aria-selected="true"]'))
  originalRow.click()
  await waitFor('Terminals still selected for the original project', () =>
    originalRow.getAttribute('aria-current') === 'true'
      && document.querySelector('#inspector-tab-terminals[aria-selected="true"]'))
}

async function selectTask(title: string) {
  const task = await waitFor(title, () => {
    if (document.querySelector('.chat__title')?.textContent === title) return true
    return Array.from(document.querySelectorAll<HTMLButtonElement>('.task'))
      .find((entry) => entry.querySelector('.task__title')?.textContent === title)
  })
  if (task !== true) {
    task.click()
    await waitFor(`selected task ${title}`, () => document.querySelector('.chat__title')?.textContent === title)
  }
}

async function prepareTask(project: bridge.ProjectSnapshot) {
  const compose = await enabledButton(`New chat in ${project.name}`)
  compose.click()
  await waitFor('draft worktree proposal', () => chatState() === 'draft' && document.querySelector('.draft__facts'))
  const [draft] = (await bridge.taskList(project.id)).tasks
  if (draft?.state !== 'draft' || draft.branch !== null || draft.worktreePath !== null) {
    throw new Error('Creating a draft should leave its branch and worktree uncreated')
  }

  const composer = await waitFor('active draft composer', () =>
    document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Prompt"]:enabled'))
  composer.focus()
  composer.value = `${WORKTREE_PROMPT} @workspace-guide`
  composer.setSelectionRange(composer.value.length, composer.value.length)
  composer.dispatchEvent(new InputEvent('input', { bubbles: true }))
  await waitFor('project file found before the draft has a worktree', () =>
    document.querySelector('[aria-label="Mention a workspace file"] [aria-selected="true"]')?.textContent?.trim() === WORKSPACE_FILE)
  press(composer, 'Enter')
  const mentionedPrompt = `${WORKTREE_PROMPT} @file("${WORKSPACE_FILE}")`
  await waitFor('file mention inserted without shifting focus', () =>
    document.activeElement === composer && composer.value.trim() === mentionedPrompt)
  press(composer, 'Enter')
  await waitFor('file mention sent through the native bridge', () =>
    document.querySelector('.chat__transcript')?.textContent?.includes(mentionedPrompt))
  await waitFor('scripted text and tool row', () => chatState() === 'waiting' &&
    document.querySelector('.entry__agent')?.textContent?.includes('local fixture') &&
    document.querySelector('.entry__tool-title')?.textContent === 'Inspect the scripted workspace')
  await waitFor('working count in the native window title', async () =>
    await getCurrentWindow().title() === `${project.name} — Shepherd (1 working)`)
  const allow = await enabledButton('Allow')
  allow.click()
  await waitFor('ready after the scripted approval', () => chatState() === 'ready')
  const { task } = await bridge.taskWorkspace(draft.id)
  if (!task.worktreePath) throw new Error('Sending the first prompt should create a worktree')
  return { id: task.id, title: task.title, worktree: task.worktreePath, branch: task.branch }
}

async function exerciseTaskSwitch(project: bridge.ProjectSnapshot, shared: bridge.TerminalSnapshot) {
  const task = await prepareTask(project)
  if (windowChecks) {
    await openInspectorTab('review')
    const composer = await waitFor('active conversation input', () =>
      document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Prompt"]:enabled'))
    composer.focus()
    const mac = navigator.platform.startsWith('Mac')
    composer.dispatchEvent(new KeyboardEvent('keydown', {
      key: mac ? 'T' : 'N', metaKey: mac, ctrlKey: !mac, shiftKey: true,
      bubbles: true, cancelable: true,
    }))
    await waitFor('shortcut opens the Terminals tab', () => {
      const tab = document.querySelector('#inspector-tab-terminals')
      return tab?.getAttribute('aria-selected') === 'true'
    })
  } else {
    await openInspectorTab('terminals')
    const start = await enabledButton('New task terminal')
    start.click()
  }
  await waitFor('three terminal canvases', () => document.querySelectorAll('.terminal-view__canvas').length === 3)
  const scoped = (await bridge.terminalList(project.id)).terminals.find((entry) => entry.taskId === task.id)
  if (scoped?.scope !== 'task' || scoped.cwd !== task.worktree) {
    throw new Error('The task terminal should run in the task worktree')
  }
  const status = () => document.querySelector('.status-bar')?.textContent ?? ''
  await waitFor('current branch and clean worktree', () =>
    status().includes(`branch ${task.branch}`) && status().includes('0 changed files'))
  await bridge.terminalWrite(scoped.id, "printf 'status fixture\\n' > shepherd-status.txt\r")
  await waitFor('dirty worktree in the status bar', () => status().includes('1 changed file'))
  await bridge.terminalWrite(scoped.id, 'rm -- shepherd-status.txt\r')
  await waitFor('clean worktree after removing the change', () => status().includes('0 changed files'))

  await typeIntoTerminal(scoped, "printf 'S4_%s:%s\\n' TASK \"${PWD##*/}\"")
  const marker = 'S4_TASK:' + task.worktree.split('/').at(-1)
  await waitFor('worktree output in the task pane', () => {
    const pane = terminalPane(scoped.title)
    return pane && terminalOutput(pane).includes(marker)
  })
  const other = await bridge.createDraft(project.id, INTERRUPTED_TASK)
  await selectTask(other.task.title)
  await waitFor('second task is still a draft', () => chatState() === 'draft')
  await waitFor('task pane hidden and shared pane still live', () => {
    const pane = terminalPane(shared.title)
    return !terminalPane(scoped.title) && pane?.querySelector('canvas') && terminalOutput(pane).includes('S4_VIEW:')
  })
  await bridge.terminalWrite(scoped.id, "printf '\\007'\r")
  await waitFor('bell on the hidden terminal’s task row', () =>
    Array.from(document.querySelectorAll('.task')).some((row) =>
      row.querySelector('.task__title')?.textContent === task.title && row.querySelector('.terminal-attention')))
  await openInspectorTab('review')
  await selectTask(task.title)
  await waitFor('scripted transcript after switching back', () =>
    document.querySelector('.entry__permission-decision')?.textContent === 'allow')
  await waitFor('bell on the task’s Terminals tab', () =>
    document.querySelector('#inspector-tab-terminals .terminal-attention'))
  await openInspectorTab('terminals')
  await waitFor('same task terminal output after switching back', () => {
    const pane = terminalPane(scoped.title)
    return pane && terminalOutput(pane).includes(marker)
  })
  await exerciseStopProcess(scoped)
}

async function openFixtureProject() {
  const add = await enabledButton('Add project')
  add.click()
  const directory = await waitFor('disposable project in the picker', () =>
    Array.from(document.querySelectorAll<HTMLButtonElement>('.picker__row'))
      .find((entry) => entry.title === __SHEPHERD_SMOKE_PROJECT__))
  directory.click()
  await waitFor('registered project in the sidebar', () => document.querySelector('.project'))
  const { projects } = await bridge.projectList()
  const project = projects.find((entry) => entry.path === __SHEPHERD_SMOKE_PROJECT__)
  if (!project) throw new Error('The fixture project was not registered')
  return project
}

function waitForCodexApproval(timeout?: number) {
  return waitFor('waiting Codex approval with command arguments', () => {
    if (chatState() === 'failed') {
      throw new Error(document.querySelector('.diagnostics pre')?.textContent ?? 'The Codex turn failed')
    }
    const entry = document.querySelector('.entry__permission')
    const argumentsText = entry?.querySelector('.entry__arguments')?.textContent ?? ''
    return chatState() === 'waiting' && argumentsText.includes('curl') &&
      argumentsText.includes('https://example.com') && entry
  }, timeout)
}

async function replayCodexApproval() {
  await openFixtureProject()
  await sendPrompt(NETWORK_PROMPT)
  const card = await waitForCodexApproval()
  const deny = Array.from(card.querySelectorAll('button')).find((entry) => entry.textContent?.trim() === 'Deny')
  if (!deny) throw new Error('The approval card has no Deny button')
  deny.click()
  await waitFor('recorded denial and completed turn', () =>
    chatState() === 'ready' &&
    document.querySelector('.entry__permission-decision')?.textContent === 'deny')
  return { ok: true, codexApprovalReplay: true }
}

async function startCodexSleep() {
  await sendPrompt('Run the shell command `sleep 25` and wait for it to finish.')
  await waitFor('running sleep tool', () => Array.from(document.querySelectorAll('.entry__tool')).some((entry) =>
    entry.querySelector('.entry__tool-title')?.textContent?.includes('sleep 25') &&
    entry.querySelector('.entry__tool-status')?.textContent === 'running'), CODEX_WAIT)
}

async function exerciseLiveCodex() {
  await openFixtureProject()
  await sendPrompt(NETWORK_PROMPT)
  const card = await waitForCodexApproval(CODEX_WAIT)
  const allow = Array.from(card.querySelectorAll('button')).find((entry) => entry.textContent?.trim() === 'Allow')
  if (!allow) throw new Error('The approval card has no Allow button')
  allow.click()
  await waitFor('recorded allowance and completed turn', () => chatState() === 'ready' &&
    document.querySelector('.entry__permission-decision')?.textContent === 'allow' &&
    document.querySelector('.entry__agent')?.textContent, CODEX_WAIT)

  await startCodexSleep()
  const cancel = await enabledButton('Cancel turn')
  cancel.click()
  await waitFor('interrupted turn settles', () => chatState() === 'ready')

  await sendPrompt('Reply with the single word: ready.')
  await waitFor('reply after cancellation', () => chatState() === 'ready' &&
    Array.from(document.querySelectorAll('.entry__agent')).at(-1)?.textContent?.trim() === 'ready', CODEX_WAIT)
  const replay = await invoke<boolean>('finish_codex_smoke')
  return { ok: true, approvalAllowed: true, cancelledRunningTool: true, followUp: true, replay }
}

async function exerciseCodexEdit() {
  await openFixtureProject()
  await sendPrompt('Append exactly the line `hello from shepherd` to `note.txt` in the current directory. Keep its existing line and change no other files. Reply with a short summary.')
  await waitFor('edited turn with streamed text and a tool row', () => chatState() === 'review' &&
    document.querySelector('.entry__agent')?.textContent &&
    document.querySelector('.entry__tool'), CODEX_WAIT)
  const review = await waitFor('Review tab', () => document.querySelector<HTMLButtonElement>('#inspector-tab-review'))
  review.click()
  const file = await waitFor('one modified file in Review', () => {
    const files = document.querySelectorAll<HTMLButtonElement>('.git-pane__file')
    const [file] = files
    return files.length === 1 && file?.textContent?.trim() === 'note.txt' && file
  })
  file.click()
  const patch = await waitFor('actual worktree patch in Review', () => {
    const text = document.querySelector('[aria-label="File diff"]')?.textContent
    return text?.split('\n').includes('+hello from shepherd') && text
  })
  const replay = await invoke<boolean>('finish_codex_smoke', { worktreePatch: patch })
  return { ok: true, streamedText: true, toolRow: true, reviewedWorktreeEdit: true, replay }
}

async function openInspectorTab(tab: 'terminals' | 'review' | 'git') {
  const control = await waitFor(`${tab} tab`, () =>
    document.querySelector<HTMLButtonElement>(`#inspector-tab-${tab}`))
  control.click()
}

async function reviewedFilePatch(path: string, addedLine: string) {
  const file = await waitFor(`${path} among the two review files`, () => {
    const files = Array.from(document.querySelectorAll<HTMLButtonElement>('.git-pane__file'))
    return files.length === 2 && files.find((entry) => entry.textContent?.trim() === path)
  })
  file.click()
  return waitFor(`review diff adding ${addedLine}`, () => {
    const patch = document.querySelector('[aria-label="File diff"]')?.textContent
    return chatState() === 'review' && patch?.split('\n').includes(`+${addedLine}`) && patch
  }, CODEX_WAIT)
}

async function fillGitText(label: 'Review note' | 'Commit message', text: string) {
  const input = await waitFor(label, () =>
    document.querySelector<HTMLTextAreaElement>(`textarea[aria-label="${label}"]`))
  input.value = text
  input.dispatchEvent(new InputEvent('input', { bubbles: true }))
}

async function exerciseCodexReviewMerge() {
  await openFixtureProject()
  await sendPrompt('Append exactly `first pass from shepherd` to note.txt and `outline from shepherd` to outline.txt, each as a new line. Keep the existing lines and change no other files.')
  await waitFor('review state after the two-file edit', () => chatState() === 'review', CODEX_WAIT)
  await openInspectorTab('review')
  const first = await reviewedFilePatch('note.txt', 'first pass from shepherd')
  const outline = await reviewedFilePatch('outline.txt', 'outline from shepherd')
  await fillGitText('Review note', 'In note.txt replace only the line `first pass from shepherd` with `reviewed by shepherd`. Leave all other lines and files unchanged.')
  const sendNote = await enabledButton('Send review note')
  sendNote.click()
  const revised = await reviewedFilePatch('note.txt', 'reviewed by shepherd')
  const unchangedOutline = await reviewedFilePatch('outline.txt', 'outline from shepherd')
  if (outline !== unchangedOutline) throw new Error('The review note unexpectedly changed outline.txt')

  await openInspectorTab('git')
  const stage = await enabledButton('Stage all')
  stage.click()
  await fillGitText('Commit message', 'Apply the review feedback')
  const commit = await enabledButton('Commit staged changes')
  commit.click()
  await waitFor('task commit in Git history', () =>
    document.querySelector('[aria-label="Git worktree"]')?.textContent?.includes('Apply the review feedback'))
  const merge = await enabledButton('Squash-merge into base')
  merge.click()
  await waitFor('squash-merge confirmation in the Git pane', () =>
    document.querySelector('[aria-label="Git worktree"] [role="status"]')?.textContent === 'Squash-merged into main.')

  await openInspectorTab('review')
  const archive = await enabledButton('Mark reviewed and archive')
  archive.click()
  await waitFor('archived task', () => chatState() === 'done')
  const worktreePatch = JSON.stringify([first + outline, revised + unchangedOutline])
  const replay = await invoke<boolean>('finish_codex_smoke', { worktreePatch })
  return { ok: true, reviewedTwoFiles: true, reviewNoteApplied: true, committed: true, squashMerged: true, archived: true, replay }
}

async function reopenArchivedTask() {
  const selected = await waitFor('restored fixture project', () =>
    document.querySelector('.project[aria-current="true"]')?.closest('.project-row'))
  selected.dispatchEvent(new PointerEvent('pointerenter', { pointerType: 'mouse' }))
  const showArchived = await enabledButton('Show archived')
  showArchived.click()
  await waitFor('selected archived task', () => chatState() === 'done')

  const { projects } = await bridge.projectList()
  const project = projects.find((entry) => entry.path === __SHEPHERD_SMOKE_PROJECT__)
  if (!project) throw new Error('The archived task project was not restored')
  const [archived] = (await bridge.taskList(project.id)).tasks
  if (archived?.state !== 'done' || !archived.worktreePath || !archived.branch) {
    throw new Error('The archived task no longer records its branch and worktree')
  }
  await openInspectorTab('review')
  const reopen = await enabledButton('Reopen')
  reopen.click()
  await waitFor('reopened task returns to review', () => chatState() === 'review')
  const { task } = await bridge.taskWorkspace(archived.id)
  if (task.branch !== archived.branch || task.worktreePath !== archived.worktreePath) {
    throw new Error('Reopen did not restore the original task branch and worktree')
  }
  return { ok: true, reopenedArchivedTask: true }
}

async function requestQuitWithLiveTask(liveTerminals = 0) {
  await enabledButton('Add project')
  await getCurrentWindow().close()
  const confirm = await enabledButton('Quit anyway')
  const terminals = liveTerminals ? ` and ${liveTerminals} terminal${liveTerminals === 1 ? '' : 's'}` : ''
  if (!confirm.closest('.banner')?.textContent?.includes(`Quit will stop 1 live task${terminals}.`)) {
    throw new Error(`The quit warning should count one live task and ${liveTerminals} live terminals`)
  }
}

async function startHangupResistantJobs(terminal: bridge.TerminalSnapshot, action: 'stop' | 'quit') {
  const pidFile = (scope: 'foreground' | 'background') => `${__SHEPHERD_SMOKE_PROJECT__}/../${action}-${scope}.pid`
  const program = String.raw`import os, signal, sys, time
from pathlib import Path
signal.signal(signal.SIGHUP, signal.SIG_IGN)
if os.fork() == 0:
    os.setpgid(0, 0)
    Path(sys.argv[2]).write_text(str(os.getpid()))
else:
    Path(sys.argv[1]).write_text(str(os.getpid()))
    deadline = time.monotonic() + 5
    while not Path(sys.argv[2]).is_file():
        if time.monotonic() >= deadline:
            raise RuntimeError('Background PTY fixture did not start')
        time.sleep(0.02)
    os.write(1, b'\nS4_' + b'JOBS_READY')
time.sleep(60)`
  const command = `python3 -u -c ${shellQuote(`exec(${JSON.stringify(program)})`)} ${shellQuote(pidFile('foreground'))} ${shellQuote(pidFile('background'))}`
  await bridge.terminalWrite(terminal.id, command + '\r')
  const output = () => {
    const pane = terminalPane(terminal.title)
    return pane ? terminalOutput(pane) : ''
  }
  try {
    await waitFor(`foreground and background jobs ready before ${action}`, () =>
      output().split('\n').some((line) => line.trim() === 'S4_JOBS_READY'))
  } catch (failure) {
    throw new Error(`${String(failure)} Terminal output: ${JSON.stringify(output().slice(-1000))}`)
  }
}

async function exerciseStopProcess(terminal: bridge.TerminalSnapshot) {
  await startHangupResistantJobs(terminal, 'stop')
  const paneButton = (label: string) => waitFor(`${label} in the task terminal`, () =>
    Array.from(terminalPane(terminal.title)?.querySelectorAll('button') ?? [])
      .find((entry) => entry.textContent === label && !entry.disabled))
  const stop = await paneButton('Stop process')
  stop.click()
  const restart = await paneButton('Restart')
  restart.click()
  await typeIntoTerminal(terminal, "printf 'S4_%s\\n' STOP_RESTARTED")
  await waitFor('fresh output after stopping and restarting the task terminal', () => {
    const pane = terminalPane(terminal.title)
    return pane && terminalOutput(pane).split('\n').some((line) => line.trim() === 'S4_STOP_RESTARTED')
  })
}

async function quitDuringCodexTurn() {
  await openFixtureProject()
  await startCodexSleep()
  await requestQuitWithLiveTask()
  const replay = await invoke<boolean>('finish_codex_smoke')
  return { ok: true, quitWithLiveCodex: true, replay }
}

async function resumeCodexAfterRestart() {
  await waitFor('interrupted Codex task in the restored conversation', () => chatState() === 'interrupted')
  const { projects } = await bridge.projectList()
  const project = projects.find((entry) => entry.path === __SHEPHERD_SMOKE_PROJECT__)
  if (!project) throw new Error('The Codex fixture project was not restored')
  const [interrupted] = (await bridge.taskList(project.id)).tasks
  if (interrupted?.state !== 'interrupted' || !interrupted.worktreePath || !interrupted.branch) {
    throw new Error('The interrupted task and worktree were not restored')
  }
  const resume = await enabledButton('Resume')
  resume.click()
  await waitFor('resumed Codex thread', () => chatState() === 'ready' &&
    document.querySelector('.entry__notice')?.textContent?.includes('resumed the previous agent session'), CODEX_WAIT)
  await sendPrompt('Reply with the single word: resumed.')
  await waitFor('Codex reply after restart', () => chatState() === 'ready' &&
    document.querySelector('.entry__agent')?.textContent?.trim() === 'resumed', CODEX_WAIT)
  const { task } = await bridge.taskWorkspace(interrupted.id)
  if (task.worktreePath !== interrupted.worktreePath || task.branch !== interrupted.branch) {
    throw new Error('Resuming the Codex thread replaced the original worktree')
  }
  const replay = await invoke<boolean>('finish_codex_smoke')
  return { ok: true, resumedCodexAfterRestart: true, retainedWorktree: true, replay }
}

async function exerciseCodexExit() {
  await openFixtureProject()
  await startCodexSleep()
  await invoke('trigger_codex_smoke_exit')
  await waitFor('failed task with a mid-turn exit row', () => chatState() === 'failed' &&
    document.querySelector('.entry__failure-title')?.textContent === 'Agent exited mid-turn')
  const diagnostics = await waitFor('Codex diagnostics', () =>
    document.querySelector<HTMLDetailsElement>('.diagnostics'))
  diagnostics.querySelector('summary')?.click()
  await waitFor('visible stderr tail', () => diagnostics.open &&
    diagnostics.querySelector('pre')?.textContent?.includes('codex app-server killed mid-turn'))
  const replay = await invoke<boolean>('finish_codex_smoke')
  return { ok: true, failedMidTurn: true, stderrVisible: true, replay }
}

async function smoke() {
  const project = await openFixtureProject()
  if (windowChecks && navigator.platform.startsWith('Mac')) {
    const dragRegion = document.querySelector('[data-tauri-drag-region]')?.getBoundingClientRect()
    const projectHeading = document.querySelector('.sidebar__header')?.getBoundingClientRect()
    if (!dragRegion?.height || !projectHeading || projectHeading.top < dragRegion.bottom) {
      throw new Error('The project header overlaps the macOS window controls')
    }
  }
  const search = await waitFor('header search with a native shortcut', () =>
    document.querySelector<HTMLInputElement>('.shell__header input[aria-keyshortcuts]:enabled'))
  const next = button('Next')?.getBoundingClientRect()
  const compact = search.getBoundingClientRect()
  if (!next || compact.left < next.right || compact.left - next.right > 10 ||
    Math.abs(compact.top + compact.height / 2 - next.top - next.height / 2) > 1) {
    throw new Error('Header search does not sit immediately after Next')
  }
  const searchOpener = await enabledButton('Add project')
  searchOpener.focus()
  const searchOnMac = navigator.platform.startsWith('Mac')
  searchOpener.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'F', metaKey: searchOnMac, shiftKey: searchOnMac, ctrlKey: !searchOnMac, altKey: !searchOnMac,
    bubbles: true, cancelable: true,
  }))
  await waitFor('focused header search', () => document.activeElement === search && search.placeholder.startsWith('Search messages'))
  press(search, 'Escape')
  await waitFor('focus returns after closing header search', () => document.activeElement === searchOpener)
  const start = await waitFor('shared terminal action', () => {
    const entry = button('New shared terminal')
    return entry && !entry.disabled && entry
  })
  start.click()
  const input = await waitFor('terminal input', () => document.querySelector<HTMLTextAreaElement>('.terminal-view__input'))
  const mac = navigator.platform.startsWith('Mac')
  const paletteBinding = mac ? 'Meta+K' : 'Control+Shift+K'
  await waitFor('platform command binding', () => button('Command palette')?.getAttribute('aria-keyshortcuts') === paletteBinding)
  input.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'k', metaKey: mac, ctrlKey: !mac, shiftKey: !mac, bubbles: true, cancelable: true,
  }))
  const palette = await waitFor('open command palette', () =>
    document.querySelector<HTMLDialogElement>('dialog[aria-label="Command palette"][open]'))
  if (!palette.textContent?.includes('Send next key to terminal')) throw new Error('The palette did not receive the core command catalog')
  palette.querySelector<HTMLButtonElement>('header button')?.click()
  await waitFor('closed command palette', () => !palette.isConnected)
  ;(await enabledButton('Settings')).click()
  ;(await enabledButton('Keyboard shortcuts')).click()
  ;(await enabledButton('Edit Command palette shortcut')).click()
  const recorder = await waitFor('keybinding recorder', () =>
    document.querySelector<HTMLInputElement>('input[data-shortcut-capture]'))
  recorder.dispatchEvent(new KeyboardEvent('keydown', { key: 'F9', bubbles: true, cancelable: true }))
  ;(await enabledButton('Save shortcut')).click()
  await waitFor('updated command binding', () => button('Command palette')?.getAttribute('aria-keyshortcuts') === 'F9')
  ;(await enabledButton('Projects')).click()
  const limit = await waitFor('project attachment limit', () =>
    document.querySelector<HTMLInputElement>('.settings input[type="number"]:enabled'))
  limit.value = '20000'
  limit.dispatchEvent(new InputEvent('input', { bubbles: true }))
  ;(await enabledButton('Save attachment limit (bytes)')).click()
  await waitFor('updated project attachment limit', async () => (await bridge.attachmentLimit(project.id)).bytes === 20_000)
  ;(await enabledButton('Settings')).click()
  const { terminals: [terminal] } = await bridge.terminalList(project.id)
  if (!terminal) throw new Error('The fixture terminal was not started')
  await exerciseAppearance(terminal.id)
  if (windowChecks) {
    await waitFor('terminal canvas frame', () => document.querySelector<HTMLCanvasElement>('.terminal-view__canvas')?.style.height)
    await exerciseInspectorResize(terminal)
    input.focus()
  }
  await bridge.terminalWrite(terminal.id, "pwd; printf '\nS4_%s\n' READY\r")
  await waitFor('project cwd and PTY output in the webview', () =>
    terminalOutput().includes(project.path) && terminalOutput().includes('S4_READY'))
  await exerciseAccessibility(input)
  await exerciseShellInterrupt(terminal.id, input)
  if (windowChecks) {
    await exerciseAlternateScreen(terminal.id, input)
    await exerciseFuzzyFinder(terminal.id, input)
    await exerciseUnclaimedControlKey(terminal.id, input)
    await exerciseComposition(terminal.id, input)
    await exerciseKitty(terminal.id, input)
  }
  if (__SHEPHERD_SMOKE_FISH__) await exerciseFish(terminal.id, input)
  await exerciseBracketedPaste(terminal.id, input)
  let windowResults = {}
  if (windowChecks) {
    await exerciseMouse(terminal.id)
    await exerciseHyperlink(terminal.id)
    const burstPaintMs = await measureBurst(terminal.id)
    const pacing = await measurePacing(terminal.id)
    await exerciseSearch(terminal, input)
    await exerciseWideCopy(terminal.id, input)
    windowResults = { ptyResize: true, scrollbackSearch: true, pacing, alternateScreen: true,
      fuzzyFinder: true, compositionEvents: true,
      kittyRoundTrip: true, sgrMouse: true, hyperlinkOpened: true, burstPaintMs }
  }
  await exerciseProjectSwitch(project, terminal)
  await exerciseSplit(project.id, terminal.id, terminal.title)
  await exerciseTaskSwitch(project, terminal)
  await selectTask(INTERRUPTED_TASK)
  await sendPrompt('Wait for approval while quitting')
  await waitFor('live task awaiting approval', () => chatState() === 'waiting')
  await startHangupResistantJobs(terminal, 'quit')
  await openInspectorTab('git')
  await requestQuitWithLiveTask(3)
  return { ok: true, quitWarning: true, taskSwitch: true, projectSwitch: true, roundTrip: true,
    splitReattach: true, commandPalette: true, savedKeybinding: true, savedProjectSettings: true, themedTerminal: true,
    terminalAccessibility: true, headerSearch: true, fileMention: true,
    fishKeyBindings: __SHEPHERD_SMOKE_FISH__, bracketedPaste: true, ...windowResults }
}

async function restore() {
  await waitFor('persisted command binding', () => button('Command palette')?.getAttribute('aria-keyshortcuts') === 'F9')
  await waitFor('persisted light theme', () => document.documentElement.dataset.theme === 'light')
  const { projects } = await bridge.projectList()
  const project = projects.find((entry) => entry.path === __SHEPHERD_SMOKE_PROJECT__)
  if (!project) throw new Error('The fixture project was not restored')
  if ((await bridge.attachmentLimit(project.id)).bytes !== 20_000) {
    throw new Error('The project attachment limit was not restored')
  }
  const terminals = (await bridge.terminalList(project.id)).terminals.filter((entry) => entry.scope === 'project')
  const [first] = terminals
  if (!first || terminals.length !== 2 || terminals.some((entry) => entry.endedAt === null)) {
    throw new Error('Both terminal records should be restored as ended')
  }
  const layout = await bridge.terminalLayout(project.id)
  if (layout.root?.type !== 'split') throw new Error('The split layout was not restored')
  await waitFor('Git tab selected after relaunch', () =>
    document.querySelector('#inspector-tab-git[aria-selected="true"]'))
  await openInspectorTab('terminals')

  const restart = await waitFor('restored terminal restart action', () => {
    const pane = terminalPane(first.title)
    return pane && Array.from(pane.querySelectorAll('button')).find((entry) => entry.textContent === 'Restart' && !entry.disabled)
  })
  restart.click()
  if (windowChecks) {
    await waitFor('restarted terminal frame', () => terminalPane(first.title)?.querySelector<HTMLCanvasElement>('canvas')?.style.height)
  }
  await typeIntoTerminal(first, 'echo S4_RESTORED')
  const { terminals: reopened } = await bridge.terminalList(project.id)
  if (!reopened.some((entry) => entry.id === first.id && entry.endedAt === null)) {
    throw new Error('Restart should reuse the restored terminal ID')
  }
  await waitFor('fresh PTY output after restore', () => {
    const pane = terminalPane(first.title)
    return pane && terminalOutput(pane).split('\n').some((line) => line.trim() === 'S4_RESTORED')
  })
  const scoped = (await bridge.terminalList(project.id)).terminals.find((entry) => entry.scope === 'task')
  const task = (await bridge.taskList(project.id)).tasks.find((entry) => entry.id === scoped?.taskId)
  if (!task?.worktreePath || !scoped || scoped.cwd !== task.worktreePath || scoped.endedAt === null) {
    throw new Error('The task terminal should restore as ended with its worktree')
  }
  await selectTask(task.title)
  const restartTask = await waitFor('restored task terminal restart action', () => {
    const pane = terminalPane(scoped.title)
    return pane && Array.from(pane.querySelectorAll('button')).find((entry) => entry.textContent === 'Restart' && !entry.disabled)
  })
  restartTask.click()
  if (windowChecks) {
    await waitFor('restarted task terminal frame', () => terminalPane(scoped.title)?.querySelector<HTMLCanvasElement>('canvas')?.style.height)
  }
  await typeIntoTerminal(scoped, "printf 'S4_%s:%s\\n' TASK_RESTORED \"${PWD##*/}\"")
  const marker = 'S4_TASK_RESTORED:' + task.worktreePath.split('/').at(-1)
  await waitFor('worktree output after task terminal restart', () => {
    const pane = terminalPane(scoped.title)
    return pane && terminalOutput(pane).includes(marker)
  })
  await selectTask(INTERRUPTED_TASK)
  await waitFor('interrupted task after relaunch', () => chatState() === 'interrupted')
  const resume = await enabledButton('Resume')
  resume.click()
  await waitFor('resumed session in the native transcript', () => chatState() === 'ready' &&
    document.querySelector('.entry__notice')?.textContent?.includes('resumed the previous agent session'))

  const search = await waitFor('header search after restart', () =>
    document.querySelector<HTMLInputElement>('.shell__header input[role="combobox"]:enabled'))
  search.focus()
  search.value = 'disposable'
  search.dispatchEvent(new InputEvent('input', { bubbles: true }))
  await waitFor('persisted Turso search hit', () =>
    Array.from(document.querySelectorAll('[aria-label="Task search results"] [role="option"]')).some((option) =>
      option.getAttribute('aria-selected') === 'true' && option.querySelector('mark')?.textContent === search.value))
  press(search, 'Enter')
  await waitFor('restored search hit focused in another task', () => {
    const entry = document.querySelector('.entry--search-hit')
    return entry && document.activeElement === entry && entry.querySelector('mark')?.textContent === 'disposable'
      && entry.textContent?.includes(WORKTREE_PROMPT)
  })
  return { ok: true, restoredLayout: true, restartedTerminal: true, restartedTaskTerminal: true,
    resumedInterruptedTask: true, restoredTaskSearch: true }
}

async function exerciseAppearance(terminalId: string) {
  let frame: bridge.TerminalFrame | undefined
  let subscriptionFailure: unknown
  const close = await bridge.subscribeTerminal(terminalId, (next) => { frame = next }, (failure) => { subscriptionFailure = failure })
  try {
    ;(await enabledButton('Settings')).click()
    ;(await enabledButton('Appearance')).click()
    const setTheme = async (theme: string) => {
      const select = await waitFor('theme choice', () =>
        document.querySelector<HTMLSelectElement>('.settings label select:enabled'))
      select.value = theme
      select.dispatchEvent(new Event('change', { bubbles: true }))
      await waitFor(`saved ${theme} theme`, () => document.documentElement.dataset.theme === theme && !select.disabled)
    }
    await setTheme('dark')
    await waitFor('dark terminal frame', () => {
      if (subscriptionFailure) throw subscriptionFailure
      const background = frame?.cells.at(-1)?.background
      return background === 0x131316 || background === 0x08080a
    })
    await setTheme('light')
    await waitFor('repainted light terminal frame', () => {
      if (subscriptionFailure) throw subscriptionFailure
      return frame?.cells.at(-1)?.background === 0xffffff
    })
    ;(await enabledButton('Settings')).click()
  } finally {
    await close()
  }
}

async function verifyBackgroundWindow() {
  const appWindow = getCurrentWindow()
  if (!document.hidden || await appWindow.isVisible() || await appWindow.isFocused()) {
    throw new Error('The background smoke window became visible or focused')
  }
}

void (async () => {
  let result
  try {
    if (windowChecks) await waitFor('the smoke window to become visible', () => !document.hidden)
    else await verifyBackgroundWindow()
    switch (__SHEPHERD_SMOKE_PHASE__) {
      case 'linux-ime':
        result = await exerciseNativeIme(30_000)
        break
      case 'macos-ime':
        result = await exerciseNativeIme(90_000)
        break
      case 'macos-dead-key':
        result = await exerciseNativeIme(90_000, [
          { text: 'é', event: 'compositionend' },
          { text: 'ü', event: 'compositionend' },
          { text: '#', event: 'input' },
        ])
        break
      case 'macos-visibility':
        result = await exerciseMacosVisibility()
        break
      case 'restore':
        result = await restore()
        break
      case 'codex-approval':
        result = await replayCodexApproval()
        break
      case 'codex-live':
        result = await exerciseLiveCodex()
        break
      case 'codex-edit':
        result = await exerciseCodexEdit()
        break
      case 'codex-before-restart':
        result = await quitDuringCodexTurn()
        break
      case 'codex-after-restart':
        result = await resumeCodexAfterRestart()
        break
      case 'codex-exit':
        result = await exerciseCodexExit()
        break
      case 'codex-review-merge':
        result = await exerciseCodexReviewMerge()
        break
      case 'reopen-archived':
        result = await reopenArchivedTask()
        break
      default:
        result = await smoke()
    }
    if (!windowChecks) {
      await verifyBackgroundWindow()
      result = { ...result, background: true }
    }
  } catch (error) {
    const message = typeof error === 'object' && error !== null && 'message' in error
      ? String(error.message)
      : String(error)
    result = { ok: false, error: message }
  }
  try {
    await fetch('/__shepherd-terminal-smoke', { method: 'POST', body: JSON.stringify(result) })
  } finally {
    const confirm = button('Quit anyway')
    if (confirm) confirm.click()
    else {
      await enabledButton('Add project').catch(() => undefined)
      await getCurrentWindow().close()
      if (!result.ok) {
        const confirm = await enabledButton('Quit anyway').catch(() => undefined)
        confirm?.click()
      }
    }
  }
})()
