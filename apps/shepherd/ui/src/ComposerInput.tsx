import { For, Show, createEffect, createSignal, createUniqueId, on, onCleanup } from 'solid-js'

import { cancelFileSearch, reserveFileSearch, searchWorkspaceFiles } from './bridge'

interface ComposerInputProps {
  value: string
  projectId: string | null
  taskId: string | null
  disabled: boolean
  placeholder: string
  inputRef: (input: HTMLTextAreaElement) => void
  onInput: (text: string, composing: boolean) => void
  onSend: () => void
}

export function ComposerInput(props: ComposerInputProps) {
  const [caret, setCaret] = createSignal<number>()
  const [focused, setFocused] = createSignal(false)
  const [composing, setComposing] = createSignal(false)
  const [dismissed, setDismissed] = createSignal(false)
  const [files, setFiles] = createSignal<string[]>([])
  const [active, setActive] = createSignal(0)
  const [searching, setSearching] = createSignal(false)
  const [error, setError] = createSignal('')
  const listId = createUniqueId()
  let input: HTMLTextAreaElement | undefined

  const mention = () => {
    const end = caret()
    if (end === undefined || !focused() || composing() || dismissed() || props.disabled || !props.projectId) return
    if (end < props.value.length && !/\s/u.test(props.value.charAt(end))) return
    const match = /(?:^|[\s([{])@([^\s"@()]*)$/u.exec(props.value.slice(0, end))
    if (match) return { start: end - match[1].length - 1, end, query: match[1] }
  }

  createEffect(on([() => props.projectId, () => props.taskId, () => mention()?.query], ([projectId, taskId, query]) => {
    setFiles([])
    setActive(0)
    setError('')
    setSearching(query !== undefined)
    if (!projectId || query === undefined) return
    let disposed = false
    let searchId: string | undefined
    const cancel = (id: string) => {
      void cancelFileSearch(id).catch((failure) => console.warn('Could not cancel file mention search', failure))
    }
    const timer = window.setTimeout(async () => {
      try {
        searchId = await reserveFileSearch()
        if (disposed) {
          cancel(searchId)
          return
        }
        const result = await searchWorkspaceFiles(searchId, projectId, taskId, { mode: 'path', query, showDotfiles: false })
        if (!disposed) setFiles(result.matches.slice(0, 12).map((file) => file.path))
      } catch (failure) {
        if (!disposed) setError(typeof failure === 'object' && failure !== null && 'message' in failure
          ? String(failure.message) : String(failure))
      } finally {
        searchId = undefined
        if (!disposed) setSearching(false)
      }
    }, 80)
    onCleanup(() => {
      disposed = true
      window.clearTimeout(timer)
      if (searchId) cancel(searchId)
    })
  }))

  function refreshCaret(element: HTMLTextAreaElement) {
    setCaret(element.selectionStart === element.selectionEnd ? element.selectionStart : undefined)
  }

  function update(element: HTMLTextAreaElement, isComposing: boolean) {
    setComposing(isComposing)
    setDismissed(false)
    props.onInput(element.value, isComposing)
    refreshCaret(element)
  }

  function choose(path: string) {
    const current = mention()
    if (!current || !input) return
    const token = `@file(${JSON.stringify(path)}) `
    const text = props.value.slice(0, current.start) + token + props.value.slice(current.end).replace(/^ /, '')
    setDismissed(true)
    props.onInput(text, false)
    input.focus()
    input.setSelectionRange(current.start + token.length, current.start + token.length)
    refreshCaret(input)
  }

  const rowId = (index: number) => `${listId}-${index}`
  function keyDown(event: KeyboardEvent) {
    if (event.defaultPrevented || event.isComposing || composing() || event.keyCode === 229) return
    if (mention()) {
      if (event.key === 'Escape') {
        event.preventDefault()
        setDismissed(true)
        return
      }
      if ((event.key === 'ArrowDown' || event.key === 'ArrowUp') && files().length) {
        event.preventDefault()
        const step = event.key === 'ArrowDown' ? 1 : -1
        const next = (active() + step + files().length) % files().length
        setActive(next)
        document.getElementById(rowId(next))?.scrollIntoView({ block: 'nearest' })
        return
      }
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault()
        const file = files()[active()]
        if (file) choose(file)
        return
      }
    }
    if (event.key === 'Enter' && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault()
      props.onSend()
    }
  }

  return (
    <>
      <textarea
        ref={(element) => { input = element; props.inputRef(element) }}
        class="composer__input"
        aria-label="Prompt"
        aria-autocomplete="list"
        aria-controls={mention() ? listId : undefined}
        aria-activedescendant={mention() && files().length ? rowId(active()) : undefined}
        placeholder={props.placeholder}
        rows={2}
        value={props.value}
        disabled={props.disabled}
        onInput={(event) => update(event.currentTarget, event.isComposing)}
        onCompositionStart={() => setComposing(true)}
        onCompositionEnd={(event) => update(event.currentTarget, false)}
        onFocus={(event) => { setFocused(true); setDismissed(false); refreshCaret(event.currentTarget) }}
        onBlur={() => setFocused(false)}
        onClick={(event) => refreshCaret(event.currentTarget)}
        onSelect={(event) => refreshCaret(event.currentTarget)}
        onKeyUp={(event) => refreshCaret(event.currentTarget)}
        onKeyDown={keyDown}
      />
      <Show when={mention()}>
        <div class="composer__mentions">
          <Show when={error() || searching() || !files().length}>
            <p class="picker__root" role="status">{error() || (searching() ? 'Searching workspace files…' : 'No workspace files match.')}</p>
          </Show>
          <ul class="picker__list" id={listId} role="listbox" aria-label="Mention a workspace file" aria-busy={searching()}>
            <For each={files()}>{(path, index) =>
              <li role="presentation">
                <button type="button" id={rowId(index())} class="picker__row"
                  classList={{ 'picker__row--active': active() === index() }}
                  role="option" aria-selected={active() === index()} tabindex={-1}
                  onMouseDown={(event) => event.preventDefault()} onClick={() => choose(path)}>
                  {path}
                </button>
              </li>
            }</For>
          </ul>
        </div>
      </Show>
    </>
  )
}
