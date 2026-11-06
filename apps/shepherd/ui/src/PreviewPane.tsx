import { For, Show, createEffect, createSignal, onCleanup } from 'solid-js'

import { openProjectPreview, type AttachmentLimit } from './bridge'

interface PreviewPaneProps {
  projectId: string
  url?: string | null
  shown: boolean
  canAddToPrompt: boolean
  attachmentLimit?: AttachmentLimit
  onAddToPrompt: (annotations: PreviewAnnotation[]) => Promise<void>
  onProjectSettings: () => void
}

export function PreviewPane(props: PreviewPaneProps) {
  const [opened, setOpened] = createSignal(false)
  createEffect(() => { if (props.shown) setOpened(true) })

  return (
    <Show when={opened() || props.shown}>
      <div class="preview-pane__view" hidden={!props.shown}>
        <Show when={props.url} keyed fallback={
          <div class="terminal-pane__bar preview-pane__bar">
            <p class="empty">Set a preview URL for this project.</p>
            <button type="button" onClick={props.onProjectSettings}>Project settings</button>
          </div>
        }>
          {(url) => <ProjectPreview {...props} url={url} />}
        </Show>
      </div>
    </Show>
  )
}

interface ElementSelection {
  url: string
  selector: string
  excerpt: string
  bounds: { x: number; y: number; width: number; height: number }
}

interface ScreenshotSelection {
  url: string
  screenshot: { file: File; x: number; y: number; width: number; height: number }
}

type PreviewSelection = ElementSelection | ScreenshotSelection
export type PreviewAnnotation = PreviewSelection & { note: string }

function selectionLabel(selection: PreviewSelection) {
  if ('selector' in selection) return selection.selector
  return `Screenshot at (${selection.screenshot.x}, ${selection.screenshot.y})`
}

function readSelection(data: unknown, origin: string): ElementSelection | undefined {
  if (typeof data !== 'object' || data === null) return
  const { url, selector, excerpt, bounds } = data as Partial<ElementSelection>
  if (typeof url !== 'string' || url.length > 8192 || typeof selector !== 'string' || !selector || selector.length > 2048
    || typeof excerpt !== 'string' || excerpt.length > 160 || !bounds || typeof bounds !== 'object') return
  let address: URL
  try { address = new URL(url) } catch { return }
  if (address.origin !== origin || address.username || address.password) return
  const { x, y, width, height } = bounds
  if (![x, y, width, height].every(Number.isFinite) || width < 0 || height < 0) return
  return { url: address.href, selector, excerpt, bounds: { x, y, width, height } }
}

function ProjectPreview(props: PreviewPaneProps & { url: string }) {
  const [revision, setRevision] = createSignal(1)
  const [opening, setOpening] = createSignal(false)
  const [error, setError] = createSignal('')
  const [annotationError, setAnnotationError] = createSignal('')
  const [selecting, setSelecting] = createSignal<'waiting' | 'ready'>()
  const [selection, setSelection] = createSignal<PreviewSelection>()
  const [screenshotMode, setScreenshotMode] = createSignal(false)
  const [adding, setAdding] = createSignal(false)
  const [note, setNote] = createSignal('')
  const [annotations, setAnnotations] = createSignal<PreviewAnnotation[]>([])
  const origin = new URL(props.url).origin
  const canFrame = origin !== window.location.origin
  let frame: HTMLIFrameElement | undefined
  let noteInput: HTMLTextAreaElement | undefined
  let selectionPort: MessagePort | undefined
  let responseTimer: number | undefined

  function stopSelection() {
    window.clearTimeout(responseTimer)
    selectionPort?.postMessage('cancel')
    selectionPort?.close()
    selectionPort = undefined
    setSelecting(undefined)
  }

  createEffect(() => { if (!props.shown) stopSelection() })
  createEffect(() => { if (selection()) noteInput?.focus() })
  onCleanup(stopSelection)

  function useScreenshot(message = '') {
    stopSelection()
    setAnnotationError(message)
    setScreenshotMode(true)
  }

  function selectElement() {
    const target = frame?.contentWindow
    if (!target) return
    const { port1, port2 } = new MessageChannel()
    selectionPort = port1
    setAnnotationError('')
    setSelecting('waiting')
    responseTimer = window.setTimeout(() => {
      useScreenshot('Element selection is unavailable for this page.')
    }, 1500)
    port1.onmessage = ({ data }) => {
      if (selectionPort !== port1) return
      if (data?.type === 'ready') {
        window.clearTimeout(responseTimer)
        setSelecting('ready')
        return
      }
      stopSelection()
      if (data?.type === 'cancelled') return
      const selected = data?.type === 'selected' && readSelection(data, origin)
      if (selected) {
        setNote('')
        setSelection(selected)
      } else {
        useScreenshot('Could not identify this element.')
      }
    }
    target.postMessage('shepherd:annotate-preview', origin, [port2])
  }

  function queueNote(event: SubmitEvent) {
    event.preventDefault()
    const selected = selection()
    const text = note().trim()
    if (!selected || !text) return
    setAnnotations((queued) => [...queued, { ...selected, note: text }])
    setSelection(undefined)
  }

  async function addToPrompt() {
    const queued = annotations()
    if (adding() || !props.canAddToPrompt || !queued.length) return
    setAdding(true)
    setAnnotationError('')
    try {
      await props.onAddToPrompt(queued)
      setAnnotations((current) => current.filter((annotation) => !queued.includes(annotation)))
    } catch (failure) {
      setAnnotationError(messageFor(failure))
    } finally {
      setAdding(false)
    }
  }

  async function open() {
    setOpening(true)
    setError('')
    try {
      await openProjectPreview(props.projectId)
    } catch (failure) {
      setError(messageFor(failure))
    } finally {
      setOpening(false)
    }
  }

  return (
    <section class="preview-pane" aria-label="Project preview">
      <div class="terminal-pane__bar preview-pane__bar">
        <p class="preview-pane__address">{props.url}</p>
        <button type="button" onClick={() => {
          stopSelection()
          setAnnotationError('')
          setScreenshotMode(false)
          setRevision((value) => value + 1)
        }}>Reload</button>
        <button type="button" onClick={open} disabled={opening()}>Open in browser</button>
        <button type="button" onClick={props.onProjectSettings}>Project settings</button>
        <Show when={selecting()} fallback={
          <button type="button" disabled={!canFrame || !!selection() || screenshotMode()} onClick={selectElement}>Annotate</button>
        }>
          <button type="button" onClick={stopSelection}>Cancel selection</button>
          <span role="status">{selecting() === 'ready' ? 'Click an element or press Escape.' : 'Waiting for the preview…'}</span>
        </Show>
        <button type="button" disabled={!!selection() || screenshotMode()} onClick={() => useScreenshot()}>Screenshot note</button>
      </div>
      <Show when={error()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
      <Show when={annotationError()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
      <Show when={selection()}>
        {(selected) => <form class="preview-pane__note" onSubmit={queueNote}>
          <p><code>{selectionLabel(selected())}</code></p>
          <label>
            Annotation note
            <textarea ref={noteInput} rows={2} maxLength={4000} value={note()} onInput={(event) => setNote(event.currentTarget.value)} />
          </label>
          <div class="terminal-pane__bar">
            <button type="submit" disabled={!note().trim()}>Queue annotation</button>
            <button type="button" onClick={() => setSelection(undefined)}>Discard note</button>
          </div>
        </form>}
      </Show>
      <Show when={annotations().length}>
        <div class="preview-pane__note">
          <ul aria-label="Queued annotations">
            <For each={annotations()}>{(annotation) => <li>
              <p><code>{selectionLabel(annotation)}</code></p>
              <p>{annotation.note}</p>
              <div class="terminal-pane__bar"><button type="button" disabled={adding()} onClick={() =>
                setAnnotations((queued) => queued.filter((entry) => entry !== annotation))}>Remove annotation</button></div>
            </li>}</For>
          </ul>
          <div class="terminal-pane__bar">
            <button type="button" onClick={addToPrompt} disabled={adding() || !props.canAddToPrompt}>Add to prompt</button>
          </div>
        </div>
      </Show>
      <Show when={screenshotMode()}>
        <ScreenshotPicker url={props.url} limit={props.attachmentLimit} onCancel={() => setScreenshotMode(false)} onSelect={(selected) => {
          setScreenshotMode(false)
          setAnnotationError('')
          setNote('')
          setSelection(selected)
        }} />
      </Show>
      <Show when={canFrame}
        fallback={<p class="entry__notice--error" role="alert">Choose a preview URL on a different origin from Shepherd.</p>}>
        <Show when={revision()} keyed>
          {(_revision) => <iframe ref={frame} title="Project website" src={props.url}
            hidden={screenshotMode()}
            sandbox="allow-scripts allow-same-origin" referrerpolicy="no-referrer" onLoad={() => {
              if (!selectionPort) return
              stopSelection()
              setAnnotationError('The preview changed. Select Annotate again.')
            }} />}
        </Show>
      </Show>
    </section>
  )
}

function ScreenshotPicker(props: { url: string; limit?: AttachmentLimit; onSelect: (selection: ScreenshotSelection) => void; onCancel: () => void }) {
  const [image, setImage] = createSignal<{ file: File; source: string }>()
  const [error, setError] = createSignal('')
  let picture: HTMLImageElement | undefined

  createEffect(() => {
    const current = image()
    if (current) onCleanup(() => URL.revokeObjectURL(current.source))
  })

  function choose(file: File | undefined) {
    if (!file) return
    setImage(undefined)
    if (!props.limit) {
      setError('The attachment limit is unavailable for this project.')
      return
    }
    if (!['image/png', 'image/jpeg'].includes(file.type) || file.size === 0 || file.size > props.limit.bytes) {
      setError(`Choose a PNG or JPEG screenshot up to ${props.limit.label}.`)
      return
    }
    setError('')
    setImage({ file, source: URL.createObjectURL(file) })
  }

  function pickPoint(event: MouseEvent) {
    const current = image()
    if (!current || !picture?.naturalWidth || !picture.naturalHeight) return
    const { naturalWidth: width, naturalHeight: height } = picture
    const rectangle = picture.getBoundingClientRect()
    if (!rectangle.width || !rectangle.height) return
    const relativeX = event.detail ? (event.clientX - rectangle.left) / rectangle.width : 0.5
    const relativeY = event.detail ? (event.clientY - rectangle.top) / rectangle.height : 0.5
    const x = Math.min(width - 1, Math.max(0, Math.floor(relativeX * width)))
    const y = Math.min(height - 1, Math.max(0, Math.floor(relativeY * height)))
    props.onSelect({ url: props.url, screenshot: { file: current.file, x, y, width, height } })
  }

  return (
    <div class="preview-pane__note">
      <p>Take a screenshot of the page in your browser, then choose the image and mark a point.</p>
      <label>
        Choose screenshot
        <input type="file" accept="image/png,image/jpeg" onChange={(event) => choose(event.currentTarget.files?.[0])} />
      </label>
      <Show when={error()}>{(message) => <p role="alert" class="entry__notice--error">{message()}</p>}</Show>
      <Show when={image()}>
        {(current) => <button type="button" class="preview-pane__screenshot" aria-label="Mark a point on the screenshot" onClick={pickPoint}>
          <img ref={picture} src={current().source} alt="Screenshot to annotate" onError={() => {
            setImage(undefined)
            setError('Could not read the screenshot image.')
          }} />
        </button>}
      </Show>
      <div class="terminal-pane__bar"><button type="button" onClick={props.onCancel}>Cancel screenshot</button></div>
    </div>
  )
}

function messageFor(failure: unknown): string {
  return failure && typeof failure === 'object' && 'message' in failure ? String(failure.message) : String(failure)
}
