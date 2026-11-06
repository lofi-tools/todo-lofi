import { For, Show, createEffect, createSignal, onCleanup } from 'solid-js'

import { openProjectPreview } from './bridge'

export function PreviewPane(props: {
  projectId: string
  url?: string | null
  shown: boolean
  canAddToPrompt: boolean
  onAddToPrompt: (text: string) => void
  onProjectSettings: () => void
}) {
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

function ProjectPreview(props: {
  url: string
  projectId: string
  shown: boolean
  canAddToPrompt: boolean
  onAddToPrompt: (text: string) => void
  onProjectSettings: () => void
}) {
  const [revision, setRevision] = createSignal(1)
  const [opening, setOpening] = createSignal(false)
  const [error, setError] = createSignal('')
  const [annotationError, setAnnotationError] = createSignal('')
  const [selecting, setSelecting] = createSignal<'waiting' | 'ready'>()
  const [element, setElement] = createSignal<ElementSelection>()
  const [note, setNote] = createSignal('')
  const [annotations, setAnnotations] = createSignal<(ElementSelection & { note: string })[]>([])
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
  createEffect(() => { if (element()) noteInput?.focus() })
  onCleanup(stopSelection)

  function selectElement() {
    const target = frame?.contentWindow
    if (!target) return
    const { port1, port2 } = new MessageChannel()
    selectionPort = port1
    setAnnotationError('')
    setSelecting('waiting')
    responseTimer = window.setTimeout(() => {
      stopSelection()
      setAnnotationError('Element selection is unavailable for this page.')
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
        setElement(selected)
      } else {
        setAnnotationError('Could not identify this element.')
      }
    }
    target.postMessage('shepherd:annotate-preview', origin, [port2])
  }

  function queueNote(event: SubmitEvent) {
    event.preventDefault()
    const selected = element()
    const text = note().trim()
    if (!selected || !text) return
    setAnnotations((queued) => [...queued, { ...selected, note: text }])
    setElement(undefined)
  }

  function addToPrompt() {
    if (!props.canAddToPrompt || !annotations().length) return
    props.onAddToPrompt(`Preview annotations\n${JSON.stringify(annotations(), null, 2)}`)
    setAnnotations([])
  }

  async function open() {
    setOpening(true)
    setError('')
    try {
      await openProjectPreview(props.projectId)
    } catch (failure) {
      setError(failure && typeof failure === 'object' && 'message' in failure ? String(failure.message) : String(failure))
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
          setRevision((value) => value + 1)
        }}>Reload</button>
        <button type="button" onClick={open} disabled={opening()}>Open in browser</button>
        <button type="button" onClick={props.onProjectSettings}>Project settings</button>
        <Show when={selecting()} fallback={
          <button type="button" disabled={!canFrame || !!element()} onClick={selectElement}>Annotate</button>
        }>
          <button type="button" onClick={stopSelection}>Cancel selection</button>
          <span role="status">{selecting() === 'ready' ? 'Click an element or press Escape.' : 'Waiting for the preview…'}</span>
        </Show>
      </div>
      <Show when={error()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
      <Show when={annotationError()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
      <Show when={element()}>
        {(selection) => <form class="preview-pane__note" onSubmit={queueNote}>
          <p><code>{selection().selector}</code></p>
          <label>
            Annotation note
            <textarea ref={noteInput} rows={2} maxLength={4000} value={note()} onInput={(event) => setNote(event.currentTarget.value)} />
          </label>
          <div class="terminal-pane__bar">
            <button type="submit" disabled={!note().trim()}>Queue annotation</button>
            <button type="button" onClick={() => setElement(undefined)}>Discard note</button>
          </div>
        </form>}
      </Show>
      <Show when={annotations().length}>
        <div class="preview-pane__note">
          <ul aria-label="Queued annotations">
            <For each={annotations()}>{(annotation) => <li>
              <p>{annotation.note}</p>
              <div class="terminal-pane__bar"><button type="button" onClick={() =>
                setAnnotations((queued) => queued.filter((entry) => entry !== annotation))}>Remove annotation</button></div>
            </li>}</For>
          </ul>
          <div class="terminal-pane__bar">
            <button type="button" onClick={addToPrompt} disabled={!props.canAddToPrompt}>Add to prompt</button>
          </div>
        </div>
      </Show>
      <Show when={canFrame}
        fallback={<p class="entry__notice--error" role="alert">Choose a preview URL on a different origin from Shepherd.</p>}>
        <Show when={revision()} keyed>
          {(_revision) => <iframe ref={frame} title="Project website" src={props.url}
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
