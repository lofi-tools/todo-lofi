import { For, Show, createEffect, createResource, createSignal } from 'solid-js'

import {
  AGENT_LABELS,
  clearPoolForce,
  clearPoolSlotCooldown,
  correctPoolQuota,
  forcePoolSlot,
  poolStatus,
  reorderPoolSlots,
  setPoolSlotEnabled,
  type PoolQuotaEntry,
  type PoolSlot,
  type PoolSlotStatus,
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

function localDateTime(value: number | null | undefined) {
  if (value == null) return ''
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  const pad = (part: number) => String(part).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
}

function parseLocalDateTime(value: string) {
  if (!value.trim()) return null
  const parsed = Date.parse(value)
  return Number.isFinite(parsed) ? parsed : undefined
}

function parseLimit(value: string) {
  const trimmed = value.trim()
  if (!trimmed) return null
  if (!/^\d+$/.test(trimmed)) return undefined
  const parsed = Number(trimmed)
  return Number.isSafeInteger(parsed) ? parsed : undefined
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

function PoolSlotEditor(props: {
  entry: PoolSlotStatus
  busy: boolean
  forced: boolean
  onCorrect: (limit: number | null, limitUnit: string | null, resetAt: number | null) => void
  onClearCooldown: () => void
  onForce: () => void
}) {
  const [limit, setLimit] = createSignal('')
  const [limitUnit, setLimitUnit] = createSignal('')
  const [reset, setReset] = createSignal('')
  const [error, setError] = createSignal('')
  const [confirming, setConfirming] = createSignal(false)
  const name = () => slotName(props.entry.slot)
  const cooldown = () => props.entry.latestQuota?.cooldownUntil ?? props.entry.slot.cooldownUntil

  createEffect(() => {
    const entry = props.entry.latestQuota
    const currentLimit = entry?.limit ?? props.entry.slot.limit
    const currentUnit = entry?.limitUnit ?? props.entry.slot.limitUnit
    const currentReset = entry?.resetAt ?? props.entry.slot.resetAt
    setLimit(currentLimit == null ? '' : String(currentLimit))
    setLimitUnit(currentUnit ?? '')
    setReset(localDateTime(currentReset))
  })

  function submit(event: SubmitEvent) {
    event.preventDefault()
    const parsedLimit = parseLimit(limit())
    if (parsedLimit === undefined) {
      setError('Limit must be a non-negative whole number.')
      return
    }
    const unit = limitUnit().trim() || null
    if (parsedLimit == null && unit) {
      setError('Enter a limit before setting a unit.')
      return
    }
    const parsedReset = parseLocalDateTime(reset())
    if (parsedReset === undefined) {
      setError('Reset must be a valid date and time.')
      return
    }
    setError('')
    props.onCorrect(parsedLimit, unit, parsedReset)
  }

  return (
    <form class="pool__correction" aria-label={`Correct ${name()}`} onSubmit={submit}>
      <label>
        Limit
        <input type="text" inputmode="numeric" aria-label={`Limit for ${name()}`} value={limit()}
          disabled={props.busy} placeholder="Unknown" onInput={(event) => setLimit(event.currentTarget.value)} />
      </label>
      <label>
        Unit
        <input type="text" aria-label={`Unit for ${name()}`} value={limitUnit()}
          disabled={props.busy} placeholder="requests" onInput={(event) => setLimitUnit(event.currentTarget.value)} />
      </label>
      <label>
        Reset
        <input type="datetime-local" step="1" aria-label={`Reset for ${name()}`} value={reset()}
          disabled={props.busy} onInput={(event) => setReset(event.currentTarget.value)} />
      </label>
      <div class="pool__correction-actions">
        <button type="submit" class="project-action" disabled={props.busy}>Save correction</button>
        <Show when={cooldown() != null}>
          <button type="button" class="project-action" disabled={props.busy}
            aria-label={`Clear cooldown for ${name()}`}
            onClick={() => { setError(''); props.onClearCooldown() }}>Clear cooldown</button>
        </Show>
      </div>
      <div class="pool__force-actions">
        <Show when={props.forced}>
          <p class="pool__hint">Forced for the next turn.</p>
        </Show>
        <Show when={!props.forced && confirming()}>
          <p class="pool__hint">Force {name()} for the next turn?</p>
          <button type="button" class="project-action" disabled={props.busy}
            aria-label={`Confirm forcing ${name()}`}
            onClick={() => { setConfirming(false); props.onForce() }}>Confirm force</button>
          <button type="button" class="project-action" disabled={props.busy}
            aria-label={`Cancel forcing ${name()}`}
            onClick={() => setConfirming(false)}>Cancel</button>
        </Show>
        <Show when={!props.forced && !confirming()}>
          <button type="button" class="project-action"
            disabled={props.busy || props.entry.availability.state !== 'available'}
            aria-label={`Force ${name()} for the next turn`}
            onClick={() => setConfirming(true)}>Force next turn</button>
          <Show when={props.entry.availability.state !== 'available'}>
            <p class="pool__hint">Only available slots can be forced.</p>
          </Show>
        </Show>
      </div>
      <Show when={error()}>
        <p class="pool__field-error" role="alert">{error()}</p>
      </Show>
    </form>
  )
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
  const forcedSlot = () => status.latest?.value?.forcedSlot ?? null
  const forcedEntry = () => slots().find((entry) => entry.slot.id === forcedSlot())
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

  function correct(slotId: string, limit: number | null, limitUnit: string | null, resetAt: number | null) {
    void mutate('correct', async () => (await correctPoolQuota(slotId, limit, limitUnit, resetAt)).reason)
  }

  function clearCooldown(slotId: string) {
    void mutate('cooldown', async () => (await clearPoolSlotCooldown(slotId)).reason)
  }

  function force(slotId: string) {
    void mutate('force', async () => (await forcePoolSlot(slotId)).reason)
  }

  function clearForce() {
    void mutate('force', async () => (await clearPoolForce()).reason)
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

      <Show when={forcedEntry()}>
        {(entry) => (
          <div class="pool__force" role="group" aria-label="Forced slot">
            <p>Forced for the next turn: {slotName(entry().slot)}.</p>
            <button type="button" class="project-action" disabled={status.loading || !!busy()}
              onClick={() => clearForce()}>Clear force</button>
          </div>
        )}
      </Show>

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
              <PoolSlotEditor entry={entry} busy={!!busy()}
                forced={entry.slot.id === forcedSlot()}
                onCorrect={(limit, limitUnit, resetAt) => correct(entry.slot.id, limit, limitUnit, resetAt)}
                onClearCooldown={() => clearCooldown(entry.slot.id)}
                onForce={() => force(entry.slot.id)} />
            </li>
          )}
        </For>
      </ol>
    </section>
  )
}
