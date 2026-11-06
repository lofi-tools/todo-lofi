import { For, Show, createEffect, createSignal, createUniqueId, on, onCleanup } from 'solid-js'

import { cancelTaskSearch, reserveTaskSearch, searchTasks, type TaskSearchHit } from './bridge'

export function TaskSearch(props: {
  open: boolean
  disabled: boolean
  shortcut?: string
  shortcutLabel?: string
  inputRef: (input: HTMLInputElement) => void
  onFocus: (opener: EventTarget | null) => void
  onBlur: () => void
  onEscape: () => void
  onSelect: (hit: TaskSearchHit) => void
}) {
  const [query, setQuery] = createSignal('')
  const [hits, setHits] = createSignal<TaskSearchHit[]>([])
  const [searching, setSearching] = createSignal(false)
  const [error, setError] = createSignal('')
  const [active, setActive] = createSignal(0)
  const listId = createUniqueId()
  const popupOpen = () => props.open && !!query().trim()

  createEffect(on(query, (value) => {
    setHits([])
    setActive(0)
    setError('')
    setSearching(!!value.trim())
    if (!value.trim()) return
    let disposed = false
    let id: string | undefined
    const cancel = (searchId: string) => {
      void cancelTaskSearch(searchId).catch((failure) => console.warn('Could not cancel task search', failure))
    }
    const debounce = window.setTimeout(async () => {
      try {
        id = await reserveTaskSearch()
        if (disposed) {
          cancel(id)
          return
        }
        const found = await searchTasks(id, value)
        if (!disposed) setHits(found)
      } catch (failure) {
        if (!disposed) setError(typeof failure === 'object' && failure !== null && 'message' in failure
          ? String(failure.message) : String(failure))
      } finally {
        id = undefined
        if (!disposed) setSearching(false)
      }
    }, 150)
    onCleanup(() => {
      disposed = true
      window.clearTimeout(debounce)
      if (id) cancel(id)
    })
  }))

  const rowId = (index: number) => `${listId}-${index}`

  function navigate(event: KeyboardEvent) {
    if (event.isComposing || event.keyCode === 229) return
    if (event.key === 'Escape') {
      event.preventDefault()
      props.onEscape()
      return
    }
    if (!hits().length) return
    if (event.key === 'Enter') {
      const hit = hits()[active()]
      if (hit) {
        event.preventDefault()
        props.onSelect(hit)
      }
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      const step = event.key === 'ArrowDown' ? 1 : -1
      const next = (active() + step + hits().length) % hits().length
      setActive(next)
      document.getElementById(rowId(next))?.scrollIntoView({ block: 'nearest' })
    }
  }

  return (
    <div class="task-search">
      <input class="task-search__input" ref={props.inputRef} role="combobox" aria-label="Search across tasks"
        aria-keyshortcuts={props.shortcut} aria-controls={popupOpen() ? listId : undefined} aria-expanded={popupOpen()}
        aria-activedescendant={popupOpen() && hits().length ? rowId(active()) : undefined} aria-autocomplete="list"
        title={props.shortcutLabel ? `Search tasks (${props.shortcutLabel})` : 'Search tasks'}
        placeholder={props.open ? 'Search messages, tasks, and projects' : props.shortcutLabel ?? 'Search'}
        value={query()} disabled={props.disabled}
        onFocus={(event) => props.onFocus(event.relatedTarget)}
        onBlur={() => { setQuery(''); props.onBlur() }}
        onInput={(event) => { if (!event.isComposing) setQuery(event.currentTarget.value) }}
        onCompositionEnd={(event) => setQuery(event.currentTarget.value)} onKeyDown={navigate} />
      <Show when={popupOpen()}>
        <div class="task-search__results">
          <Show when={error()}>
            {(message) => <p class="picker__root" role="alert">{message()}</p>}
          </Show>
          <Show when={searching() || (!error() && !hits().length)}>
            <p class="picker__root" role="status">{searching() ? 'Searching tasks…' : 'No tasks match.'}</p>
          </Show>
          <ul class="picker__list" id={listId} role="listbox" aria-label="Task search results" aria-busy={searching()}>
            <For each={hits()}>
              {(hit, index) => <li role="presentation">
                <button type="button" id={rowId(index())} class="picker__row task-search__hit" tabindex={-1}
                  classList={{ 'picker__row--active': active() === index() }} role="option" aria-selected={active() === index()}
                  onMouseDown={(event) => event.preventDefault()} onClick={() => props.onSelect(hit)}>
                  <span><strong>{hit.taskTitle}</strong> <span class="task-search__project">{hit.projectName}</span></span>
                  <span>{hit.snippet.before}<Show when={hit.snippet.matched}><mark class="search-mark">{hit.snippet.matched}</mark></Show>{hit.snippet.after}</span>
                </button>
              </li>}
            </For>
          </ul>
        </div>
      </Show>
    </div>
  )
}
