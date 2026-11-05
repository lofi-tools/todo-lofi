import { getCurrentWindow } from '@tauri-apps/api/window'

import * as bridge from '../src/bridge'

declare const __SHEPHERD_SMOKE_FISH__: boolean
declare const __SHEPHERD_SMOKE_PROJECT__: string
declare const __SHEPHERD_SMOKE_PHASE__: string

export function button(label: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll('button'))
    .find((entry) => (entry.getAttribute('aria-label') ?? entry.textContent?.trim()) === label)
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
  await runPtyProbe(terminalId, String.raw`
os.write(1, b'\x1b[>3u\x1b[?u')
expect(b'\x1b[?3u')
os.write(1, b'\r\nS4_' + b'KITTY_READY\r\n')
expect(b'\x1b[27u\x1b[27;1:3u')
os.write(1, b'\r\nS4_' + b'KITTY_PASS\r\n')
`, '1b5b3c75')
  await waitFor('Kitty negotiation reply in the PTY', () => terminalOutput().includes('S4_KITTY_READY'))
  for (const type of ['keydown', 'keyup']) {
    input.dispatchEvent(new KeyboardEvent(type, {
      key: 'Escape', code: 'Escape', bubbles: true, cancelable: true,
    }))
  }
  await waitFor('Kitty key press and release in the PTY', () => terminalOutput().includes('S4_KITTY_PASS'))
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
expect(b'\x1b[<65;1;1M')
os.write(1, b'\r\nS4_' + b'MOUSE_PASS\r\n')
`, '1b5b3f313030306c1b5b3f313030366c')
  await waitFor('PTY SGR mouse mode', () => terminalOutput().includes('S4_MOUSE_READY'))
  const canvas = await waitFor('terminal mouse surface', () => document.querySelector('.terminal-view__canvas'))
  const { left, top } = canvas.getBoundingClientRect()
  canvas.dispatchEvent(new WheelEvent('wheel', {
    bubbles: true, cancelable: true, deltaMode: WheelEvent.DOM_DELTA_LINE, deltaY: 1,
    clientX: left + 1, clientY: top + 1,
  }))
  await waitFor('SGR wheel report in the PTY', () => terminalOutput().includes('S4_MOUSE_PASS'))
}

function paneWidth(title: string): number {
  const canvas = terminalPane(title)?.querySelector('canvas')
  return parseFloat(canvas?.style.width ?? '') || 0
}

async function ptyColumns(terminalId: string, title: string, marker: string): Promise<number> {
  await bridge.terminalWrite(terminalId, "printf 'S4_%s:%s\\n' " + marker + ' "$(stty size)"\r')
  const size = new RegExp('S4_' + marker + ':(\\d+) (\\d+)')
  const match = await waitFor(marker + ' PTY dimensions', () => {
    const pane = terminalPane(title)
    return pane && terminalOutput(pane).match(size)
  })
  return Number(match[2])
}

async function exerciseSplit(projectId: string, terminalId: string, title: string) {
  const initialWidth = paneWidth(title)
  const initialColumns = await ptyColumns(terminalId, title, 'INITIAL_SIZE')
  const split = await waitFor('split action', () => {
    const entry = button('Split right')
    return entry && !entry.disabled && entry
  })
  split.click()
  const divider = await waitFor('live split divider', () => document.querySelector<HTMLElement>('.terminal-divider'))
  await waitFor('both canvases in the split', () => document.querySelectorAll('.terminal-view__canvas').length === 2)
  await waitFor('split layout save', () => !button('Split right')?.disabled)
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

async function exerciseSearch(terminalId: string) {
  const latest = await bridge.searchTerminal(terminalId, 'S4_T0', null, 'previous')
  if (!latest) throw new Error('Paced output was missing from scrollback')
  const previous = await bridge.searchTerminal(terminalId, 'S4_T0', latest.start, 'previous')
  if (!previous || previous.start.line >= latest.start.line) {
    throw new Error('Searching backward should select an earlier marker')
  }
  const next = await bridge.searchTerminal(terminalId, 'S4_T0', previous.end, 'next')
  if (!next || next.start.line !== latest.start.line || next.start.column !== latest.start.column) {
    throw new Error('Searching forward should return to the later marker')
  }
}

async function selectTask(title: string) {
  const task = await waitFor(title, () => {
    if (document.querySelector('.chat__title')?.textContent === title) return true
    return Array.from(document.querySelectorAll<HTMLButtonElement>('.task'))
      .find((entry) => entry.querySelector('.task__title')?.textContent === title)
  })
  if (task !== true) task.click()
}

async function prepareTask(projectId: string) {
  const draft = await bridge.createDraft(projectId, 'Terminal scope')
  const waiting = await bridge.sendPrompt(draft.task.id, 'Prepare the disposable terminal worktree')
  if (waiting.task.state !== 'waiting' || !waiting.task.worktreePath) {
    throw new Error('The scripted task should have a worktree and await approval')
  }
  const ready = await bridge.resolveApproval(draft.task.id, 'approval-1', 'allow')
  if (ready.task.state !== 'ready') throw new Error('The scripted task should finish after approval')
  await selectTask(ready.task.title)
  return { id: ready.task.id, title: ready.task.title, worktree: waiting.task.worktreePath }
}

async function exerciseTaskSwitch(projectId: string, shared: bridge.TerminalSnapshot) {
  const task = await prepareTask(projectId)
  const start = await waitFor('task terminal action for the worktree', () => {
    const entry = button('New task terminal')
    return entry && !entry.disabled && entry
  })
  start.click()
  await waitFor('three terminal canvases', () => document.querySelectorAll('.terminal-view__canvas').length === 3)
  const scoped = (await bridge.terminalList(projectId)).find((entry) => entry.taskId === task.id)
  if (scoped?.scope !== 'task' || scoped.cwd !== task.worktree) {
    throw new Error('The task terminal should run in the task worktree')
  }

  await bridge.terminalWrite(scoped.id, "printf 'S4_%s:%s\\n' TASK \"${PWD##*/}\"\r")
  const marker = 'S4_TASK:' + task.worktree.split('/').at(-1)
  await waitFor('worktree output in the task pane', () => {
    const pane = terminalPane(scoped.title)
    return pane && terminalOutput(pane).includes(marker)
  })
  const other = await bridge.createDraft(projectId, 'Other terminal scope')
  await selectTask(other.task.title)
  await waitFor('task pane hidden and shared pane still live', () => {
    const pane = terminalPane(shared.title)
    return !terminalPane(scoped.title) && pane?.querySelector('canvas') && terminalOutput(pane).includes('S4_VIEW:')
  })
  await selectTask(task.title)
  await waitFor('same task terminal output after switching back', () => {
    const pane = terminalPane(scoped.title)
    return pane && terminalOutput(pane).includes(marker)
  })
}

async function openFixtureProject() {
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
  return project
}

async function replayCodexApproval() {
  await openFixtureProject()
  const input = await waitFor('active composer', () => {
    const textarea = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Prompt"]')
    return textarea && !textarea.disabled && textarea
  })
  input.value = "Run this shell command and report only the status code: curl -s -o /dev/null -w '%{http_code}' https://example.com"
  input.dispatchEvent(new InputEvent('input', { bubbles: true }))
  const send = await waitFor('enabled Send', () => {
    const entry = button('Send')
    return entry && !entry.disabled && entry
  })
  send.click()

  const chatState = () => document.querySelector('.chat__state')?.textContent
  const card = await waitFor('waiting Codex approval with command arguments', () => {
    if (chatState() === 'failed') {
      throw new Error(document.querySelector('.diagnostics pre')?.textContent ?? 'The Codex replay failed')
    }
    const entry = document.querySelector('.entry__permission')
    const argumentsText = entry?.querySelector('.entry__arguments')?.textContent ?? ''
    return chatState() === 'waiting' && argumentsText.includes('curl') &&
      argumentsText.includes('https://example.com') && entry
  })
  const deny = Array.from(card.querySelectorAll('button')).find((entry) => entry.textContent?.trim() === 'Deny')
  if (!deny) throw new Error('The approval card has no Deny button')
  deny.click()
  await waitFor('recorded denial and completed turn', () =>
    chatState() === 'ready' &&
    document.querySelector('.entry__permission-decision')?.textContent === 'deny')
  return { ok: true, codexApprovalReplay: true }
}

async function smoke() {
  const project = await openFixtureProject()
  if (navigator.platform.startsWith('Mac')) {
    const dragRegion = document.querySelector('[data-tauri-drag-region]')?.getBoundingClientRect()
    const projectHeading = document.querySelector('.sidebar__header')?.getBoundingClientRect()
    if (!dragRegion?.height || !projectHeading || projectHeading.top < dragRegion.bottom) {
      throw new Error('The project header overlaps the macOS window controls')
    }
  }
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
  if (__SHEPHERD_SMOKE_FISH__) await exerciseFish(terminal.id, input)
  await exerciseBracketedPaste(terminal.id, input)
  await exerciseMouse(terminal.id)
  const burstPaintMs = await measureBurst(terminal.id)
  const pacing = await measurePacing(terminal.id)
  await exerciseSearch(terminal.id)
  await exerciseSplit(project.id, terminal.id, terminal.title)
  await exerciseTaskSwitch(project.id, terminal)
  return { ok: true, taskSwitch: true, roundTrip: true, splitReattach: true, ptyResize: true, scrollbackSearch: true, pacing, alternateScreen: true, compositionEvents: true, kittyRoundTrip: true, fishKeyBindings: __SHEPHERD_SMOKE_FISH__, bracketedPaste: true, sgrMouse: true, burstPaintMs }
}

async function restore() {
  const { projects } = await bridge.projectList()
  const project = projects.find((entry) => entry.path === __SHEPHERD_SMOKE_PROJECT__)
  if (!project) throw new Error('The fixture project was not restored')
  const terminals = (await bridge.terminalList(project.id)).filter((entry) => entry.scope === 'project')
  const [first] = terminals
  if (!first || terminals.length !== 2 || terminals.some((entry) => entry.endedAt === null)) {
    throw new Error('Both terminal records should be restored as ended')
  }
  const layout = await bridge.terminalLayout(project.id)
  if (layout.root?.type !== 'split') throw new Error('The split layout was not restored')

  const restart = await waitFor('restored terminal restart action', () => {
    const pane = terminalPane(first.title)
    return pane && Array.from(pane.querySelectorAll('button')).find((entry) => entry.textContent === 'Restart' && !entry.disabled)
  })
  restart.click()
  await waitFor('restarted terminal frame', () => terminalPane(first.title)?.querySelector<HTMLCanvasElement>('canvas')?.style.height)
  const reopened = await bridge.terminalList(project.id)
  if (!reopened.some((entry) => entry.id === first.id && entry.endedAt === null)) {
    throw new Error('Restart should reuse the restored terminal ID')
  }
  await bridge.terminalWrite(first.id, "printf 'S4_%s\\n' RESTORED\r")
  await waitFor('fresh PTY output after restore', () => {
    const pane = terminalPane(first.title)
    return pane && terminalOutput(pane).includes('S4_RESTORED')
  })
  const task = (await bridge.taskList(project.id)).tasks.find((entry) => entry.title === 'Terminal scope')
  const scoped = (await bridge.terminalList(project.id)).find((entry) => entry.scope === 'task')
  if (!task?.worktreePath || !scoped || scoped.taskId !== task.id || scoped.cwd !== task.worktreePath || scoped.endedAt === null) {
    throw new Error('The task terminal should restore as ended with its worktree')
  }
  await selectTask(task.title)
  const restartTask = await waitFor('restored task terminal restart action', () => {
    const pane = terminalPane(scoped.title)
    return pane && Array.from(pane.querySelectorAll('button')).find((entry) => entry.textContent === 'Restart' && !entry.disabled)
  })
  restartTask.click()
  await waitFor('restarted task terminal frame', () => terminalPane(scoped.title)?.querySelector<HTMLCanvasElement>('canvas')?.style.height)
  await bridge.terminalWrite(scoped.id, "printf 'S4_%s:%s\\n' TASK_RESTORED \"${PWD##*/}\"\r")
  const marker = 'S4_TASK_RESTORED:' + task.worktreePath.split('/').at(-1)
  await waitFor('worktree output after task terminal restart', () => {
    const pane = terminalPane(scoped.title)
    return pane && terminalOutput(pane).includes(marker)
  })
  return { ok: true, restoredLayout: true, restartedTerminal: true, restartedTaskTerminal: true }
}

void (async () => {
  let result
  try {
    switch (__SHEPHERD_SMOKE_PHASE__) {
      case 'restore':
        result = await restore()
        break
      case 'codex-approval':
        result = await replayCodexApproval()
        break
      default:
        result = await smoke()
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
    await getCurrentWindow().close()
  }
})()
