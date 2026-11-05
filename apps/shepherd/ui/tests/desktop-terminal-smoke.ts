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

export function terminalOutput(): string {
  return document.querySelector('.terminal-view__text')?.textContent ?? ''
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
  const burstPaintMs = await measureBurst(terminal.id)
  return { ok: true, roundTrip: true, burstPaintMs }
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
