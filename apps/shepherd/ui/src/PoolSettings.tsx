import { For, Show, createMemo, createResource, createSignal, onCleanup, onMount } from 'solid-js'
import { AGENT_LABELS, agentAvailability, detectedHarnesses, harnessLabel, harnessVersionStatus, type Agent, type PoolAgent } from './bridge'
import { AgentIcon } from './AgentIcon'

export const DEFAULT_AGENT_PRIORITY: Agent[] = ['codex', 'opencode', 'pi', 'claude', 'freebuff', 'antigravity']

export function agentPriority(saved?: PoolAgent[]): PoolAgent[] {
  return [...new Set(saved ?? DEFAULT_AGENT_PRIORITY)]
}

function HarnessCard(props: {
  agent: PoolAgent
  label: string
  version: string
  selected: boolean
  editable: boolean
  dragging?: boolean
  preview?: { x: number; y: number; width: number }
  rowRef?: (row: HTMLLIElement) => void
  onSelect?: () => void
  onSettings?: () => void
  onMove?: (direction: number) => void
  onPointerDown?: (event: PointerEvent) => void
}) {
  return <li ref={props.rowRef} class="pool__agent" classList={{
    'pool__agent--editable': props.editable,
    'pool__agent--dragging': props.dragging,
    'pool__agent--selected': props.selected,
    'pool__drag-card': !!props.preview,
  }} style={props.preview ? { left: `${props.preview.x}px`, top: `${props.preview.y}px`, width: `${props.preview.width}px` } : undefined}
    tabIndex={props.preview ? undefined : 0} onKeyDown={(event) => {
      if (event.target !== event.currentTarget || props.preview) return
      if (props.editable && event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) { event.preventDefault(); props.onMove?.(event.key === 'ArrowUp' ? -1 : 1) }
      else if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); props.onSelect?.() }
    }}
    onClick={() => props.onSelect?.()}
    onDragStart={(event) => event.preventDefault()}
    onPointerDown={(event) => props.onPointerDown?.(event)}>
    <span class="pool__drag-handle" aria-hidden="true">⠿</span>
    <span class="pool__agent-label"><span class="settings__harness-name"><AgentIcon agent={props.agent} />{props.label}</span><span class="empty">{props.version}</span></span>
    <button type="button" class="pool__settings" aria-label={`${AGENT_LABELS[props.agent]} harness settings`} title="Harness settings" onClick={(event) => { event.stopPropagation(); props.onSettings?.() }}><GearIcon /></button>
  </li>
}

export function PoolSettings(props: { priority?: PoolAgent[]; editable: boolean; selected?: PoolAgent; onSelect: (agent: PoolAgent) => void; onChange: (priority: PoolAgent[]) => Promise<void> }) {
  const [failure, setFailure] = createSignal('')
  const [draft, setDraft] = createSignal<PoolAgent[]>()
  const [dragged, setDragged] = createSignal<PoolAgent>()
  const [saving, setSaving] = createSignal(false)
  const [dragCard, setDragCard] = createSignal<{ x: number; y: number; width: number }>()
  const rows = new Map<PoolAgent, HTMLLIElement>()
  let list!: HTMLOListElement
  let pointer: { id: number; agent: PoolAgent; x: number; y: number; offsetX: number; offsetY: number; width: number; active: boolean; inside: boolean } | undefined
  let suppressClick = false
  const [discovery] = createResource(async () => {
    try {
      const [harnesses, availability] = await Promise.all([detectedHarnesses(), agentAvailability()])
      return { installed: new Set([...harnesses.map((harness) => harness.agent), ...DEFAULT_AGENT_PRIORITY.filter((agent) => availability[agent])]), harnesses, availability, error: '' }
    } catch (error) {
      return { installed: new Set<PoolAgent>(), harnesses: [], availability: undefined, error: String(error) }
    }
  })
  const priority = createMemo(() => draft() ?? agentPriority(props.priority))
  const installed = createMemo(() => priority().filter((agent) => discovery()?.installed.has(agent) && discovery()?.availability?.[agent as Agent]))
  const unimplemented: PoolAgent[] = ['kilocode']
  const notAdded = createMemo(() => DEFAULT_AGENT_PRIORITY.filter((agent) => discovery()?.installed.has(agent) && !priority().includes(agent)))
  const unavailable = createMemo(() => [...new Set([...priority().filter((agent) => discovery()?.installed.has(agent) && !discovery()?.availability?.[agent as Agent]), ...unimplemented])])
  const detectedVersion = (agent: PoolAgent) => {
    const harness = discovery()?.harnesses.find((entry) => entry.agent === agent)
    return harnessVersionStatus(agent, harness?.version, harness?.path)
  }
  const detectedLabel = (agent: PoolAgent) => harnessLabel(agent, discovery()?.harnesses.filter((harness) => harness.agent === agent).map((harness) => harness.path) ?? [])
  const editable = () => props.editable && !saving()

  function reorder(agent: PoolAgent, target: PoolAgent, after: boolean) {
    const ordered = installed()
    const from = ordered.indexOf(agent)
    let to = ordered.indexOf(target) + (after ? 1 : 0)
    if (from < 0 || to < 0) return
    if (from < to) to -= 1
    if (from === to) return
    const before = new Map([...rows].map(([id, row]) => [id, row.getBoundingClientRect().top]))
    const nextVisible = [...ordered]
    nextVisible.splice(from, 1)
    nextVisible.splice(to, 0, agent)
    let visibleIndex = 0
    const next = priority().map((id) => discovery()?.installed.has(id) && discovery()?.availability?.[id as Agent] ? nextVisible[visibleIndex++] : id)
    setDraft(next)
    queueMicrotask(() => {
      for (const [id, row] of rows) {
        const previous = before.get(id)
        if (previous === undefined) continue
        const distance = previous - row.getBoundingClientRect().top
        if (!distance) continue
        row.style.transition = 'none'
        row.style.transform = `translateY(${distance}px)`
        row.getBoundingClientRect()
        row.style.transition = ''
        row.style.transform = ''
      }
    })
  }

  async function save() {
    const next = draft()
    if (!next) return
    setSaving(true)
    setFailure('')
    try { await props.onChange(next) }
    catch (error) { setFailure(String(error)) }
    finally { setDraft(undefined); setSaving(false) }
  }

  function move(agent: PoolAgent, direction: number) {
    const other = installed()[installed().indexOf(agent) + direction]
    if (!other) return
    reorder(agent, other, direction > 0)
    void save()
  }

  async function add(agent: PoolAgent) {
    setSaving(true)
    setFailure('')
    try { await props.onChange([...priority(), agent]) }
    catch (error) { setFailure(String(error)) }
    finally { setSaving(false) }
  }

  function finishDrag(commit: boolean) {
    const current = pointer
    if (!current) return
    pointer = undefined
    if (list.hasPointerCapture(current.id)) list.releasePointerCapture(current.id)
    setDragged(undefined)
    setDragCard(undefined)
    suppressClick = true
    setTimeout(() => { suppressClick = false }, 0)
    if (commit && current.active && current.inside) void save()
    else if (current.active) setDraft(undefined)
    else if (commit) props.onSelect(current.agent)
  }

  onMount(() => {
    const cancel = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && pointer) {
        event.preventDefault()
        finishDrag(false)
      }
    }
    window.addEventListener('keydown', cancel)
    onCleanup(() => window.removeEventListener('keydown', cancel))
  })

  return <section class="pool" aria-label="Agent priority" aria-busy={discovery.loading || saving()}>
    <p class="empty">Drag agents to set the order used for new conversations. Use Alt and the arrow keys to reorder with the keyboard.</p>
    <Show when={discovery.loading}><p class="empty">Detecting agents…</p></Show>
    <Show when={discovery()?.error}>{(error) => <p role="alert">Could not detect agents: {error()}</p>}</Show>
    <Show when={!discovery.loading && !discovery()?.error && !installed().length}><p class="empty">No available agents detected.</p></Show>
    <Show when={failure()}>{(error) => <p role="alert">Could not save agent priority: {error()}</p>}</Show>
    <ol ref={list} class="pool__list" onPointerMove={(event) => {
      const current = pointer
      if (!current || current.id !== event.pointerId) return
      if (!current.active) {
        if (Math.hypot(event.clientX - current.x, event.clientY - current.y) < 4) return
        current.active = true
        setDraft(agentPriority(props.priority))
        setDragged(current.agent)
      }
      event.preventDefault()
      setDragCard({ x: event.clientX - current.offsetX, y: event.clientY - current.offsetY, width: current.width })
      const bounds = list.getBoundingClientRect()
      current.inside = event.clientX >= bounds.left && event.clientX <= bounds.right && event.clientY >= bounds.top && event.clientY <= bounds.bottom
      if (!current.inside) return
      for (const agent of installed()) {
        const row = rows.get(agent)
        if (!row || agent === current.agent) continue
        const rowBounds = row.getBoundingClientRect()
        if (event.clientY >= rowBounds.top && event.clientY <= rowBounds.bottom) {
          reorder(current.agent, agent, event.clientY >= rowBounds.top + rowBounds.height / 2)
          break
        }
      }
    }} onPointerUp={(event) => { if (pointer?.id === event.pointerId) finishDrag(true) }}
      onPointerCancel={(event) => { if (pointer?.id === event.pointerId) finishDrag(false) }}
      onLostPointerCapture={(event) => { if (pointer?.id === event.pointerId) finishDrag(false) }}>
      <For each={installed()}>{(agent) => <HarnessCard agent={agent} label={detectedLabel(agent)} version={detectedVersion(agent)} selected={props.selected === agent} editable={editable()} dragging={dragged() === agent}
        rowRef={(row) => rows.set(agent, row)} onSelect={() => { if (!suppressClick) props.onSelect(agent) }} onSettings={() => props.onSelect(agent)} onMove={(direction) => move(agent, direction)}
        onPointerDown={(event) => {
          if (!editable() || pointer || !event.isPrimary || event.button !== 0 || (event.target instanceof Element && event.target.closest('button'))) return
          const row = rows.get(agent)
          if (!row) return
          const bounds = row.getBoundingClientRect()
          pointer = { id: event.pointerId, agent, x: event.clientX, y: event.clientY, offsetX: event.clientX - bounds.left, offsetY: event.clientY - bounds.top, width: bounds.width, active: false, inside: true }
          list.setPointerCapture(event.pointerId)
        }} />}</For>
    </ol>
    <Show when={notAdded().length > 0}><h4 class="pool__section-heading">Detected harnesses</h4><ul class="pool__list">
      <For each={notAdded()}>{(agent) => <li class="pool__agent" classList={{ 'pool__agent--selected': props.selected === agent }}>
        <button type="button" class="pool__agent-label" onClick={() => props.onSelect(agent)}><span class="settings__harness-name"><AgentIcon agent={agent} />{harnessLabel(agent, discovery()?.harnesses.filter((harness) => harness.agent === agent).map((harness) => harness.path) ?? [])}</span><span class="empty">{detectedVersion(agent)}</span></button>
        <button type="button" class="project-action" disabled={!editable()} onClick={() => void add(agent)}>Add</button>
        <button type="button" class="pool__settings" aria-label={`${AGENT_LABELS[agent]} harness settings`} title="Harness settings" onClick={() => props.onSelect(agent)}><GearIcon /></button>
      </li>}</For>
    </ul></Show>
    <Show when={unavailable().length > 0}><h4 class="pool__section-heading">Unavailable</h4><ul class="pool__list">
      <For each={unavailable()}>{(agent) => <li class="pool__agent" classList={{ 'pool__agent--selected': props.selected === agent, 'pool__agent--unimplemented': unimplemented.includes(agent) }} onClick={() => props.onSelect(agent)}>
        <button type="button" class="pool__agent-label" onClick={(event) => { event.stopPropagation(); props.onSelect(agent) }}><span class="settings__harness-name"><AgentIcon agent={agent} />{AGENT_LABELS[agent]}</span><span class="empty">{unimplemented.includes(agent) ? `Unimplemented${discovery()?.installed.has(agent) ? ' · Detected' : ''}` : 'Not available to Shepherd'}</span><Show when={discovery()?.installed.has(agent)}><span class="empty">{detectedVersion(agent)}</span></Show></button>
        <Show when={unimplemented.includes(agent) && !priority().includes(agent)}><button type="button" class="project-action" disabled={!editable()} onClick={(event) => { event.stopPropagation(); void add(agent) }}>Add</button></Show>
        <button type="button" class="pool__settings" aria-label={`${AGENT_LABELS[agent]} harness settings`} title="Harness settings" onClick={(event) => { event.stopPropagation(); props.onSelect(agent) }}><GearIcon /></button>
      </li>}</For>
    </ul></Show>
    <Show when={dragged() && dragCard()}>{(card) => <ul class="pool__drag-layer" aria-hidden="true" inert>
      <HarnessCard agent={dragged()!} label={detectedLabel(dragged()!)} version={detectedVersion(dragged()!)} selected={props.selected === dragged()} editable={editable()} preview={card()} />
    </ul>}</Show>
  </section>
}

function GearIcon() {
  return <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="4" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.55V21a2 2 0 1 1-4 0v-.08A1.7 1.7 0 0 0 8.97 19.4a1.7 1.7 0 0 0-1.88.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.55-1H3a2 2 0 1 1 0-4h.08A1.7 1.7 0 0 0 4.6 8.97a1.7 1.7 0 0 0-.34-1.88l-.06-.06A2 2 0 1 1 7.03 4.2l.06.06A1.7 1.7 0 0 0 8.97 4.6 1.7 1.7 0 0 0 10 3.05V3a2 2 0 1 1 4 0v.08A1.7 1.7 0 0 0 15.03 4.6a1.7 1.7 0 0 0 1.88-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 8.97 1.7 1.7 0 0 0 20.95 10H21a2 2 0 1 1 0 4h-.08A1.7 1.7 0 0 0 19.4 15Z" />
          </svg>
}
