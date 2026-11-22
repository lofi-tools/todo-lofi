import { For, Show, createEffect, createMemo, createSignal, createUniqueId, onCleanup } from 'solid-js'

import type { Entry } from './bridge'

export function MessageRail(props: { entries: Entry[]; transcript: () => HTMLElement | undefined; onNavigate: (entryId: string) => void }) {
  const messages = createMemo(() => props.entries.filter((entry) => entry.kind.type === 'userMessage'), undefined, {
    equals: (previous, next) => previous.length === next.length && previous.every((entry, index) => entry === next[index]),
  })
  const [preview, setPreview] = createSignal<{ id: string; text: string; top: number; min: number; max: number }>()
  const [visible, setVisible] = createSignal<Set<string>>(new Set())
  const tooltipId = createUniqueId()
  let rail: HTMLElement | undefined

  createEffect(() => {
    const transcript = props.transcript()
    const entries = messages()
    setVisible(new Set(entries[0] ? [entries[0].id] : []))
    if (!transcript) return
    let active = true
    const measure = () => {
      if (!active) return
      const viewport = transcript.getBoundingClientRect()
      const next = new Set<string>()
      let preceding: string | undefined
      for (const entry of entries) {
        const element = transcript.querySelector(`[data-entry-id="${CSS.escape(entry.id)}"]`)
        if (!element) continue
        const bounds = element.getBoundingClientRect()
        if (bounds.top <= viewport.top) preceding = entry.id
        if (bounds.bottom > viewport.top && bounds.top < viewport.bottom) next.add(entry.id)
      }
      if (!next.size) {
        const fallback = preceding ?? entries[0]?.id
        if (fallback) next.add(fallback)
      }
      setVisible(next)
    }
    const resize = new ResizeObserver(measure)
    resize.observe(transcript)
    queueMicrotask(() => {
      if (!active) return
      const content = transcript.querySelector('.transcript')
      if (content) resize.observe(content)
      measure()
    })
    transcript.addEventListener('scroll', measure, { passive: true })
    onCleanup(() => { active = false; resize.disconnect(); transcript.removeEventListener('scroll', measure) })
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
