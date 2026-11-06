import { Show, createSignal } from 'solid-js'

import { openProjectPreview } from './bridge'

export function PreviewPane(props: {
  projectId: string
  url?: string | null
  onProjectSettings: () => void
}) {
  return (
    <Show when={props.url} keyed fallback={
      <div class="terminal-pane__bar preview-pane__bar">
        <p class="empty">Set a preview URL for this project.</p>
        <button type="button" onClick={props.onProjectSettings}>Project settings</button>
      </div>
    }>
      {(url) => <ProjectPreview url={url} projectId={props.projectId} onProjectSettings={props.onProjectSettings} />}
    </Show>
  )
}

function ProjectPreview(props: { url: string; projectId: string; onProjectSettings: () => void }) {
  const [revision, setRevision] = createSignal(1)
  const [opening, setOpening] = createSignal(false)
  const [error, setError] = createSignal('')

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
        <button type="button" onClick={() => setRevision((value) => value + 1)}>Reload</button>
        <button type="button" onClick={open} disabled={opening()}>Open in browser</button>
        <button type="button" onClick={props.onProjectSettings}>Project settings</button>
      </div>
      <Show when={error()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
      <Show when={new URL(props.url).origin !== window.location.origin}
        fallback={<p class="entry__notice--error" role="alert">Choose a preview URL on a different origin from Shepherd.</p>}>
        <Show when={revision()} keyed>
          {(_revision) => <iframe title="Project website" src={props.url} sandbox="allow-scripts allow-same-origin" referrerpolicy="no-referrer" />}
        </Show>
      </Show>
    </section>
  )
}
