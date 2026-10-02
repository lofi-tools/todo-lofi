import { For, Show, createMemo, createSignal } from 'solid-js'
import { css } from 'styled-system/css'
import { AlertCircle, Check, Clock, Repeat, X } from 'web-design-system/icons'
import ViewShell, {
  action,
  card,
  chip,
  fieldHint,
  note,
  row,
  rowBetween,
} from './ViewShell'

type EntryStatus = 'synced' | 'failed' | 'incoming' | 'pending'

interface Entry {
  id: string
  app: 'GitHub' | 'Todoist'
  summary: string
  detail: string
  time: string
  status: EntryStatus
  attempts: number
}

const list = css({
  display: 'flex',
  flexDirection: 'column',
  m: '0',
  p: '0',
  listStyle: 'none',
})

const entry = css({
  display: 'flex',
  alignItems: 'center',
  gap: '3',
  px: '3',
  py: '2.5',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
  '&:last-child': { borderBottomWidth: '0' },
})

const marker = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '6',
  h: '6',
  flexShrink: '0',
  borderRadius: 'full',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  color: 'fg.subtle',
  '&[data-status="synced"]': { color: 'success' },
  '&[data-status="failed"]': { color: 'danger' },
  '&[data-status="incoming"]': { color: 'accent.text' },
})

const entryBody = css({ flex: '1', minW: '0', display: 'flex', flexDirection: 'column', gap: '0.5' })

const entryTitle = css({ fontSize: 'xs', color: 'fg.default' })

const entryDetail = css({
  fontSize: '2xs',
  color: 'fg.subtle',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
})

const entryTime = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
  flexShrink: '0',
  fontVariantNumeric: 'tabular-nums',
})

const FILTERS: { id: EntryStatus | 'all'; label: string }[] = [
  { id: 'all', label: 'Everything' },
  { id: 'synced', label: 'Synced' },
  { id: 'incoming', label: 'Incoming' },
  { id: 'failed', label: 'Failed' },
]

const entries: Entry[] = [
  {
    id: 'h1',
    app: 'Todoist',
    summary: 'Water the plants · due date updated',
    detail: 'Todoist → Taskstream',
    time: '2m ago',
    status: 'incoming',
    attempts: 0,
  },
  {
    id: 'h2',
    app: 'Todoist',
    summary: '4 tasks updated in both apps, nothing conflicted',
    detail: 'Local → Todoist',
    time: '18m ago',
    status: 'synced',
    attempts: 0,
  },
  {
    id: 'h3',
    app: 'GitHub',
    summary: 'Pull request #482 merged',
    detail: 'todo-list · taskstream/review-482',
    time: '41m ago',
    status: 'synced',
    attempts: 0,
  },
  {
    id: 'h4',
    app: 'GitHub',
    summary: 'Publish the release notes',
    detail: 'Local edit could not reach GitHub: 502 from the API',
    time: '1h ago',
    status: 'failed',
    attempts: 2,
  },
  {
    id: 'h5',
    app: 'Todoist',
    summary: 'Reorder the inbox',
    detail: 'Replay stalled on a missing project id',
    time: '3h ago',
    status: 'failed',
    attempts: 4,
  },
  {
    id: 'h6',
    app: 'Todoist',
    summary: 'Feed the dog · completed',
    detail: 'Todoist → Taskstream',
    time: 'Yesterday',
    status: 'incoming',
    attempts: 0,
  },
  {
    id: 'h7',
    app: 'GitHub',
    summary: 'Opened taskstream/migrate-schema',
    detail: 'todo-list · 3 commits',
    time: 'Yesterday',
    status: 'synced',
    attempts: 0,
  },
]

/** The sync history page: the log the desktop shows in its history pane. */
export default function SyncHistoryView() {
  const [filter, setFilter] = createSignal<EntryStatus | 'all'>('all')
  const [hidden, setHidden] = createSignal<string[]>([])

  const failed = createMemo(() => entries.filter((row) => row.status === 'failed'))

  const visible = createMemo(() =>
    entries.filter(
      (row) =>
        !hidden().includes(row.id) && (filter() === 'all' || row.status === filter()),
    ),
  )

  return (
    <ViewShell
      title="Sync history"
      subtitle={
        failed().length > 0
          ? `History — ${failed().length} failed sync operation${failed().length === 1 ? '' : 's'} waiting to be retried`
          : 'History — synced operations and incoming changes'
      }
      actions={
        <button type="button" class={action} onClick={() => setHidden([])}>
          <Repeat size={11} />
          Re-read the sync log
        </button>
      }
    >
      <div class={rowBetween}>
        <div class={row} role="group" aria-label="Filter">
          <For each={FILTERS}>
            {(option) => (
              <button
                type="button"
                class={chip}
                data-active={filter() === option.id ? '' : undefined}
                aria-pressed={filter() === option.id}
                onClick={() => setFilter(option.id)}
              >
                {option.label}
              </button>
            )}
          </For>
        </div>
        <span class={fieldHint}>The log keeps 30 days; failed operations retry for a week.</span>
      </div>

      <article class={card}>
        <Show when={visible().length > 0} fallback={<p class={note}>Nothing synced yet.</p>}>
          <ul class={list}>
            <For each={visible()}>
              {(row) => (
                <li class={entry}>
                  <span class={marker} data-status={row.status}>
                    <Show when={row.status === 'synced'}>
                      <Check size={12} />
                    </Show>
                    <Show when={row.status === 'failed'}>
                      <AlertCircle size={12} />
                    </Show>
                    <Show when={row.status === 'incoming'}>
                      <Clock size={12} />
                    </Show>
                    <Show when={row.status === 'pending'}>
                      <Repeat size={12} />
                    </Show>
                  </span>

                  <span class={entryBody}>
                    <span class={entryTitle}>{row.summary}</span>
                    <span class={entryDetail}>
                      {row.app} · {row.detail}
                    </span>
                  </span>

                  <Show when={row.status === 'failed'}>
                    <span class={chip} data-active="">
                      {row.attempts} attempt{row.attempts === 1 ? '' : 's'}
                    </span>
                    <button type="button" class={action}>
                      Retry
                    </button>
                  </Show>

                  <span class={entryTime}>{row.time}</span>

                  <button
                    type="button"
                    class={action}
                    data-muted=""
                    aria-label={`Hide ${row.summary}`}
                    onClick={() => setHidden((current) => [...current, row.id])}
                  >
                    <X size={11} />
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </article>
    </ViewShell>
  )
}
