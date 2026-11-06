import { For, Show, createMemo, createSignal, createUniqueId, onCleanup, onMount } from 'solid-js'

export interface PaletteEntry {
  id: string
  label: string
  category?: string
  detail?: string
  binding?: string
  disabledReason?: string
}

function scoreMatch(label: string, category: string, query: string) {
  let score = 0
  for (const word of query.toLowerCase().trim().split(/\s+/).filter(Boolean)) {
    const exact = label.indexOf(word)
    if (exact >= 0) {
      score += exact + (exact > 0 && label[exact - 1] !== ' ' ? 10 : 0)
      continue
    }
    let next = 0
    let gaps = 0
    for (const character of word) {
      const index = label.indexOf(character, next)
      if (index < 0) {
        gaps = Infinity
        break
      }
      gaps += index - next
      next = index + 1
    }
    score += Math.min(20 + gaps, category.includes(word) ? 40 : Infinity)
  }
  return score
}

export function CommandPalette(props: {
  title: string
  entries: PaletteEntry[]
  onClose: () => void
  onSelect: (entry: PaletteEntry) => void
}) {
  const [query, setQuery] = createSignal('')
  const [active, setActive] = createSignal(0)
  const entries = createMemo(() => {
    if (!query().trim()) return props.entries
    return props.entries
      .map((entry) => ({
        entry,
        score: scoreMatch(entry.label.toLowerCase(), (entry.category ?? '').toLowerCase(), query()),
      }))
      .filter(({ score }) => Number.isFinite(score))
      .sort((left, right) => left.score - right.score)
      .map(({ entry }) => entry)
  })
  const listId = createUniqueId()
  const rowId = (index: number) => `${listId}-${index}`
  let dialog: HTMLDialogElement | undefined
  let input: HTMLInputElement | undefined

  onMount(() => {
    dialog?.showModal()
    input?.focus()
    onCleanup(() => dialog?.close())
  })

  function updateQuery(value: string) {
    setQuery(value)
    setActive(0)
  }

  function choose(entry?: PaletteEntry) {
    if (entry && !entry.disabledReason) props.onSelect(entry)
  }

  function navigate(event: KeyboardEvent) {
    if (event.isComposing || event.keyCode === 229 || !entries().length) return
    if (event.key === 'Enter') {
      event.preventDefault()
      choose(entries()[active()])
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      const step = event.key === 'ArrowDown' ? 1 : -1
      const next = (active() + step + entries().length) % entries().length
      setActive(next)
      document.getElementById(rowId(next))?.scrollIntoView({ block: 'nearest' })
    }
  }

  return (
    <dialog class="picker picker-dialog" aria-label={props.title} ref={dialog}
      onCancel={(event) => { event.preventDefault(); props.onClose() }}>
      <header class="picker__header">
        <h3>{props.title}</h3>
        <button type="button" onClick={props.onClose}>Close</button>
      </header>
      <input ref={input} class="picker__filter" role="combobox" aria-label={`Filter ${props.title.toLowerCase()}`}
        aria-controls={listId} aria-expanded={entries().length > 0} aria-autocomplete="list"
        aria-activedescendant={entries()[active()] ? rowId(active()) : undefined}
        value={query()} placeholder="Type to filter"
        onInput={(event) => { if (!event.isComposing) updateQuery(event.currentTarget.value) }}
        onCompositionEnd={(event) => updateQuery(event.currentTarget.value)} onKeyDown={navigate} />
      <Show when={!entries().length}><p class="picker__root" role="status">No matches.</p></Show>
      <ul class="picker__list" id={listId} role="listbox" aria-label={`${props.title} results`}>
        <For each={entries()}>
          {(entry, index) => (
            <li role="presentation">
              <button type="button" role="option" tabindex={-1} id={rowId(index())} class="picker__row"
                classList={{ 'picker__row--active': active() === index() }}
                aria-selected={active() === index()} aria-disabled={!!entry.disabledReason}
                onMouseDown={(event) => event.preventDefault()} onClick={() => choose(entry)}>
                <span class="palette__entry">
                  <strong>{entry.label}</strong>
                  <Show when={entry.category || entry.detail}>
                    <span class="palette__detail">{entry.category ?? entry.detail}</span>
                  </Show>
                  <Show when={entry.disabledReason}>
                    <span class="palette__detail">{entry.disabledReason}</span>
                  </Show>
                </span>
                <Show when={entry.binding}>{(binding) => <kbd>{binding()}</kbd>}</Show>
              </button>
            </li>
          )}
        </For>
      </ul>
    </dialog>
  )
}
