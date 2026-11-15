import { For, Show, createMemo, createResource, createSignal } from 'solid-js'
import { AGENT_LABELS, agentAvailability, detectedHarnesses, type Agent } from './bridge'
import { AgentIcon } from './AgentIcon'

export const DEFAULT_AGENT_PRIORITY: Agent[] = ['codex', 'opencode', 'pi', 'claude', 'freebuff']

export function agentPriority(saved?: Agent[]): Agent[] {
  return [...new Set([...(saved ?? []), ...DEFAULT_AGENT_PRIORITY])]
}

export function PoolSettings(props: { priority?: Agent[]; editable: boolean; onChange: (priority: Agent[]) => Promise<void> }) {
  const [failure, setFailure] = createSignal('')
  const [draft, setDraft] = createSignal<Agent[]>()
  const [dragged, setDragged] = createSignal<Agent>()
  const [saving, setSaving] = createSignal(false)
  const rows = new Map<Agent, HTMLLIElement>()
  const [discovery] = createResource(async () => {
    try {
      const [harnesses, availability] = await Promise.all([detectedHarnesses(), agentAvailability()])
      return { installed: new Set([...harnesses.map((harness) => harness.agent), ...DEFAULT_AGENT_PRIORITY.filter((agent) => availability[agent])]), availability, error: '' }
    } catch (error) {
      return { installed: new Set<Agent>(), availability: undefined, error: String(error) }
    }
  })
  const priority = createMemo(() => draft() ?? agentPriority(props.priority))
  const installed = createMemo(() => priority().filter((agent) => discovery()?.installed.has(agent)))
  const editable = () => props.editable && !saving()

  function reorder(agent: Agent, target: Agent, after: boolean) {
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
    const next = priority().map((id) => discovery()?.installed.has(id) ? nextVisible[visibleIndex++] : id)
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

  function move(agent: Agent, direction: number) {
    const other = installed()[installed().indexOf(agent) + direction]
    if (!other) return
    reorder(agent, other, direction > 0)
    void save()
  }

  return <section class="pool" aria-label="Agent priority" aria-busy={discovery.loading || saving()}>
    <p class="empty">Drag agents to set the order used for new conversations.</p>
    <Show when={discovery.loading}><p class="empty">Detecting agents…</p></Show>
    <Show when={discovery()?.error}>{(error) => <p role="alert">Could not detect agents: {error()}</p>}</Show>
    <Show when={!discovery.loading && !discovery()?.error && !installed().length}><p class="empty">No agents detected. Configure a harness in Settings.</p></Show>
    <Show when={failure()}>{(error) => <p role="alert">Could not save agent priority: {error()}</p>}</Show>
    <ol class="pool__list" onDragOver={(event) => { if (dragged()) event.preventDefault() }} onDrop={(event) => {
      if (!dragged()) return
      event.preventDefault()
      setDragged(undefined)
      void save()
    }}>
      <For each={installed()}>{(agent, index) => <li ref={(row) => rows.set(agent, row)} class="pool__agent" classList={{ 'pool__agent--dragging': dragged() === agent }}
        draggable={editable()} onDragStart={(event) => {
          if (!editable() || (event.target instanceof Element && event.target.closest('button'))) { event.preventDefault(); return }
          setDraft(agentPriority(props.priority))
          setDragged(agent)
          event.dataTransfer?.setData('text/plain', agent)
          if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move'
        }} onDragOver={(event) => {
          const source = dragged()
          if (!source || source === agent) return
          event.preventDefault()
          const bounds = event.currentTarget.getBoundingClientRect()
          reorder(source, agent, event.clientY >= bounds.top + bounds.height / 2)
        }} onDragEnd={() => { if (dragged()) { setDragged(undefined); setDraft(undefined) } }}>
        <span class="pool__drag-handle" aria-hidden="true">⠿</span>
        <div class="pool__agent-label"><div class="settings__harness-name"><AgentIcon agent={agent} />{AGENT_LABELS[agent]}</div>
          <Show when={!discovery()?.availability?.[agent]}><p class="empty">Not available to Shepherd</p></Show></div>
        <div class="pool__agent-actions"><button class="project-action" type="button" aria-label={`Move ${AGENT_LABELS[agent]} up`} disabled={!editable() || index() === 0} onClick={() => move(agent, -1)}>↑</button>{' '}
        <button class="project-action" type="button" aria-label={`Move ${AGENT_LABELS[agent]} down`} disabled={!editable() || index() === installed().length - 1} onClick={() => move(agent, 1)}>↓</button></div>
      </li>}</For>
    </ol>
  </section>
}
