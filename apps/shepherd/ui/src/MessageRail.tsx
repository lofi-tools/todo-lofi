import { For, Show, createMemo, createSignal, createUniqueId } from 'solid-js'

import type { Entry } from './bridge'

export function MessageRail(props: { entries: Entry[]; onNavigate: (entryId: string) => void }) {
  const messages = createMemo(() => props.entries.filter((entry) => entry.kind.type === 'userMessage'))
  const [preview, setPreview] = createSignal<{ id: string; text: string; top: number }>()
  const tooltipId = createUniqueId()
  let rail: HTMLElement | undefined

  function showPreview(entry: Entry, button: HTMLButtonElement) {
    if (entry.kind.type !== 'userMessage' || !rail) return
    const railBounds = rail.getBoundingClientRect()
    const buttonBounds = button.getBoundingClientRect()
    setPreview({ id: entry.id, text: entry.kind.data.text.trim() || 'Message with attachments', top: buttonBounds.top - railBounds.top + buttonBounds.height / 2 })
  }

  return <Show when={messages().length > 0}>
    <nav ref={rail} class="message-rail" aria-label="Your messages">
      <div class="message-rail__items" onScroll={() => setPreview(undefined)}>
        <For each={messages()}>{(entry, index) => <button type="button" class="message-rail__item"
          aria-label={`Go to your message ${index() + 1}`}
          aria-describedby={preview()?.id === entry.id ? tooltipId : undefined}
          onPointerEnter={(event) => showPreview(entry, event.currentTarget)}
          onPointerLeave={() => setPreview(undefined)}
          onFocus={(event) => showPreview(entry, event.currentTarget)}
          onBlur={() => setPreview(undefined)}
          onClick={() => props.onNavigate(entry.id)}><span aria-hidden="true" /></button>}</For>
      </div>
      <Show when={preview()}>{(item) => <div id={tooltipId} role="tooltip" class="chat__menu-card message-rail__preview"
        style={{ top: `clamp(0px, ${item().top - 24}px, max(0px, 100% - 80px))` }}>{item().text}</div>}</Show>
    </nav>
  </Show>
}
