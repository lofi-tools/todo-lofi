import { For, Show, createResource, createSignal } from 'solid-js'

import {
  AGENT_LABELS,
  poolStatus,
  reorderPoolSlots,
  setPoolSlotEnabled,
  type PoolQuotaEntry,
  type PoolSlot,
} from './bridge'
import { reportError } from './diagnostics'

const AVAILABILITY_LABELS = {
  available: 'Available',
  exhausted: 'Exhausted',
  'cooling-down': 'Cooling down',
  disabled: 'Disabled',
  unhealthy: 'Unhealthy',
  unknown: 'Unknown',
} as const

function describeError(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'message' in error) return String(error.message)
  return String(error)
}

function formatMoment(value: number | null | undefined) {
  if (value == null) return 'Unknown'
  return new Date(value).toISOString()
}

function quotaSummary(entry: PoolQuotaEntry | null) {
  if (!entry) return 'Unknown'
  const unit = entry.limitUnit ?? 'units'
  if (entry.used != null && entry.limit != null) return `${entry.used} of ${entry.limit} ${unit}`
  if (entry.limit != null) return `${entry.limit} ${unit} limit; usage unknown`
  return 'Unknown'
}

function slotError(slot: PoolSlot, entry: PoolQuotaEntry | null) {
  return slot.lastError ?? entry?.error ?? 'None'
}

function slotName(slot: PoolSlot) {
  return `${slot.providerName} · ${slot.modelName}`
}

export function PoolSettings() {
  const [busy, setBusy] = createSignal<string>()
  const [notice, setNotice] = createSignal('')
  const [actionError, setActionError] = createSignal('')
  const [status, { refetch }] = createResource(async () => {
    try {
      return { value: await poolStatus(), error: '' }
    } catch (error) {
      reportError('pool_status', error)
      return { value: undefined, error: describeError(error) }
    }
  })

  const slots = () => status.latest?.value?.slots ?? []
  const loadError = () => status.latest?.error ?? ''
  const visibleError = () => actionError() || loadError()

  async function mutate(action: string, operation: () => Promise<string>) {
    if (busy()) return
    setBusy(action)
    setNotice('')
    setActionError('')
    try {
      setNotice(await operation())
      await refetch()
    } catch (error) {
      reportError('pool_control', error)
      setActionError(describeError(error))
    } finally {
      setBusy(undefined)
    }
  }

  function move(slotId: string, offset: number) {
    const current = slots()
    const ids = current.map((entry) => entry.slot.id)
    const from = ids.indexOf(slotId)
    const to = from + offset
    if (from < 0 || to < 0 || to >= ids.length) return
    ;[ids[from], ids[to]] = [ids[to], ids[from]]
    void mutate('reorder', async () => (await reorderPoolSlots(ids)).reason)
  }

  function toggle(slot: PoolSlot) {
    void mutate('toggle', async () => (await setPoolSlotEnabled(slot.id, !slot.enabled)).reason)
  }

  return (
    <section class="pool" aria-label="Agent pool" aria-busy={status.loading || !!busy()}>
      <div class="pool__intro">
        <p class="empty">
          Slots are ordered for future task-start selection. Changes here do not change a running
          task.
        </p>
        <button type="button" class="project-action" disabled={status.loading || !!busy()}
          onClick={() => { setActionError(''); void refetch() }}>Refresh pool</button>
      </div>

      <Show when={notice()}>
        <p class="pool__notice" role="status">{notice()}</p>
      </Show>
      <Show when={visibleError()}>
        <div class="pool__error" role="alert">
          <span>{visibleError()}</span>
          <button type="button" class="project-action" disabled={status.loading || !!busy()}
            onClick={() => { setActionError(''); void refetch() }}>Retry pool</button>
        </div>
      </Show>

      <Show when={status.loading && !status.latest}>
        <p class="empty">Loading pool…</p>
      </Show>
      <Show when={!status.loading && status.latest?.value && slots().length === 0}>
        <p class="empty">No pool slots are configured.</p>
      </Show>

      <ol class="pool__list">
        <For each={slots()}>
          {(entry, index) => (
            <li class="pool__slot">
              <div class="pool__heading">
                <div>
                  <h4>{slotName(entry.slot)}</h4>
                  <p class="empty">{AGENT_LABELS[entry.slot.agent]} · priority {index() + 1}</p>
                </div>
                <span class={`pool__availability pool__availability--${entry.availability.state}`}>
                  {AVAILABILITY_LABELS[entry.availability.state]}
                </span>
              </div>
              <p class="pool__reason">{entry.availability.reason}</p>
              <dl class="pool__facts">
                <div><dt>Quota</dt><dd>{quotaSummary(entry.latestQuota)}</dd></div>
                <div><dt>Reset</dt><dd>{formatMoment(entry.latestQuota?.resetAt ?? entry.slot.resetAt)}</dd></div>
                <div><dt>Current task</dt><dd>{entry.currentRequest ?? 'None'}</dd></div>
                <div><dt>Last error</dt><dd>{slotError(entry.slot, entry.latestQuota)}</dd></div>
                <div><dt>Last used</dt><dd>{formatMoment(entry.slot.lastUsedAt)}</dd></div>
              </dl>
              <div class="pool__actions">
                <button type="button" class="project-action"
                  aria-label={`Move ${slotName(entry.slot)} up`}
                  disabled={!!busy() || index() === 0}
                  onClick={() => move(entry.slot.id, -1)}>Move up</button>
                <button type="button" class="project-action"
                  aria-label={`Move ${slotName(entry.slot)} down`}
                  disabled={!!busy() || index() === slots().length - 1}
                  onClick={() => move(entry.slot.id, 1)}>Move down</button>
                <button type="button" class="project-action"
                  aria-label={`${entry.slot.enabled ? 'Disable' : 'Enable'} ${slotName(entry.slot)}`}
                  disabled={!!busy()}
                  onClick={() => toggle(entry.slot)}>{entry.slot.enabled ? 'Disable' : 'Enable'}</button>
              </div>
            </li>
          )}
        </For>
      </ol>
    </section>
  )
}
