import { For, Show, createEffect, createMemo, createSignal, createUniqueId, onCleanup } from 'solid-js'

import type { Entry } from './bridge'

export function MessageRail(props: { entries: Entry[]; transcript: () => HTMLElement | undefined; onNavigate: (entryId: string) => void }) {
  const messages = createMemo(() => props.entries.filter((entry) => entry.kind.type === 'userMessage'))
  const [preview, setPreview] = createSignal<{ id: string; text: string; top: number; min: number; max: number }>()
  const [visible, setVisible] = createSignal<Set<string>>(new Set())
  const tooltipId = createUniqueId()
  let rail: HTMLElement | undefined

  createEffect(() => {
    const transcript = props.transcript()
    const entries = messages()
    setVisible(new Set<string>())
    if (!transcript) return
    let active = true
    const observer = new IntersectionObserver((changes) => {
      if (!active) return
      setVisible((current) => {
        const next = new Set(current)
        for (const change of changes) {
          const id = (change.target as HTMLElement).dataset.entryId
          if (!id) continue
          if (change.isIntersecting && change.intersectionRect.height > 0) next.add(id)
          else next.delete(id)
        }
        return next
      })
    }, { root: transcript })
    queueMicrotask(() => {
      if (!active) return
      for (const entry of entries) {
        const element = transcript.querySelector(`[data-entry-id="${CSS.escape(entry.id)}"]`)
        if (element) observer.observe(element)
      }
    })
    onCleanup(() => { active = false; observer.disconnect() })
  })

  function showPreview(entry: Entry, button: HTMLButtonElement) {
    if (entry.kind.type !== 'userMessage' || !rail) return
    const railBounds = rail.getBoundingClientRect()
    const buttonBounds = button.getBoundingClientRect()
    const containerBounds = rail.parentElement?.getBoundingClientRect() ?? railBounds
    setPreview({ id: entry.id, text: entry.kind.data.text.trim() || 'Message with attachments', top: buttonBounds.top - railBounds.top + buttonBounds.height / 2, min: containerBounds.top - railBounds.top + 32, max: containerBounds.bottom - railBounds.top - 32 })
  }

  return <Show when={messages().length > 0}>
    <nav ref={rail} class="message-rail" aria-label="Your messages">
      <div class="message-rail__items" onScroll={() => setPreview(undefined)}>
        <For each={messages()}>{(entry, index) => <button type="button" class="message-rail__item"
          classList={{ 'message-rail__item--visible': visible().has(entry.id) }}
          aria-label={`Go to your message ${index() + 1}`}
          aria-describedby={preview()?.id === entry.id ? tooltipId : undefined}
          onPointerEnter={(event) => showPreview(entry, event.currentTarget)}
          onPointerLeave={() => setPreview(undefined)}
          onFocus={(event) => showPreview(entry, event.currentTarget)}
          onBlur={() => setPreview(undefined)}
          onClick={() => props.onNavigate(entry.id)}><span aria-hidden="true" /></button>}</For>
      </div>
      <Show when={preview()}>{(item) => <div id={tooltipId} role="tooltip" class="chat__menu-card message-rail__preview"
        style={{ top: `clamp(${item().min}px, ${item().top}px, ${item().max}px)` }}>{item().text}</div>}</Show>
    </nav>
  </Show>
}
