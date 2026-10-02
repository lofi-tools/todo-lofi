import { For, Show, createSignal } from 'solid-js'
import { css } from 'styled-system/css'
import { Check, Play, X, Zap } from 'web-design-system/icons'
import ViewShell, {
  action,
  card,
  cardHead,
  cardMeta,
  cardTitle,
  chip,
  field,
  fieldHint,
  fieldLabel,
  note,
  row,
  rowBetween,
} from './ViewShell'

type RunStatus = 'running' | 'waiting' | 'complete' | 'cancelled'

interface Run {
  id: string
  task: string
  phase: string
  round: number
  status: RunStatus
  branch: string
  detail?: string
}

interface Automation {
  id: string
  label: string
  enabled: boolean
  boundTags: string[]
  trigger: string
  runs: Run[]
}

const progress = css({
  display: 'flex',
  alignItems: 'center',
  gap: '1',
  flexShrink: '0',
})

const step = css({
  h: '1',
  w: '8',
  borderRadius: 'full',
  bg: 'border.subtle',
  '&[data-state="done"]': { bg: 'success' },
  '&[data-state="active"]': { bg: 'accent' },
})

const runRow = css({
  display: 'flex',
  flexDirection: 'column',
  gap: '2',
  p: '3',
  borderRadius: 'md',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.elevated',
})

const runHead = css({ display: 'flex', alignItems: 'center', gap: '2' })

const runTitle = css({ flex: '1', minW: '0', fontSize: 'xs', color: 'fg.default' })

const runMeta = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
})

/** The automations page: what is enabled, what tags it manages, and its runs. */
export default function AutomationsView() {
  const [automations, setAutomations] = createSignal<Automation[]>([
    {
      id: 'coding-runs',
      label: 'Coding runs',
      enabled: true,
      boundTags: ['work', 'release'],
      trigger: 'A task starting in a bound tag',
      runs: [
        {
          id: 'run-482',
          task: 'Ship the release',
          phase: 'Implement',
          round: 2,
          status: 'running',
          branch: 'taskstream/ship-release',
          detail: 'Editing docs/release-notes.md',
        },
        {
          id: 'run-479',
          task: 'Review PR #482',
          phase: 'Plan',
          round: 1,
          status: 'waiting',
          branch: 'taskstream/review-482',
          detail: 'Waiting for your approval to start the implementation round.',
        },
        {
          id: 'run-471',
          task: 'Migrate database schema',
          phase: 'Implement',
          round: 2,
          status: 'complete',
          branch: 'taskstream/migrate-schema',
        },
      ],
    },
    {
      id: 'daily-digest',
      label: 'Daily digest',
      enabled: false,
      boundTags: ['life admin'],
      trigger: 'Every day at 8:00 AM',
      runs: [],
    },
  ])

  const [cleanup, setCleanup] = createSignal<string | null>(null)

  const patch = (id: string, change: (automation: Automation) => Automation) =>
    setAutomations((list) => list.map((entry) => (entry.id === id ? change(entry) : entry)))

  const setRunStatus = (automationId: string, runId: string, status: RunStatus) =>
    patch(automationId, (automation) => ({
      ...automation,
      runs: automation.runs.map((run) => (run.id === runId ? { ...run, status } : run)),
    }))

  return (
    <ViewShell
      title="Automations"
      subtitle="Work the app runs on your behalf, and the runs waiting on you"
    >
      <For each={automations()}>
        {(automation) => (
          <article class={card}>
            <div class={cardHead}>
              <span class={css({ display: 'inline-flex', color: 'fg.muted' })}>
                <Zap size={14} />
              </span>
              <span class={cardTitle}>{automation.label}</span>
              <button
                type="button"
                class={chip}
                data-active={automation.enabled ? '' : undefined}
                aria-pressed={automation.enabled}
                onClick={() =>
                  patch(automation.id, (entry) => ({ ...entry, enabled: !entry.enabled }))
                }
              >
                {automation.enabled ? <Check size={10} /> : null}
                {automation.enabled ? 'Enabled' : 'Enable'}
              </button>
              <span class={cardMeta}>
                Managing {automation.boundTags.map((tag) => `#${tag}`).join(', ')}
              </span>
            </div>

            <div class={runMeta}>
              <span>Fire: {automation.trigger}</span>
            </div>

            <Show
              when={automation.enabled}
              fallback={<p class={note}>Not enabled. Enable it to let its triggers fire.</p>}
            >
              <Show
                when={automation.runs.length > 0}
                fallback={<p class={note}>No runs yet. Start a run: its steps appear in the task list.</p>}
              >
                <For each={automation.runs}>
                  {(run) => (
                    <div class={runRow}>
                      <div class={runHead}>
                        <span class={runTitle}>{run.task}</span>
                        <span class={chip} data-active={run.status === 'running' ? '' : undefined}>
                          {run.status === 'running'
                            ? 'Running'
                            : run.status === 'waiting'
                              ? 'Waiting'
                              : run.status === 'complete'
                                ? 'Complete'
                                : 'Cancelled'}
                        </span>
                      </div>

                      <div class={runMeta}>
                        Round {run.round} · {run.phase}
                        <div class={progress} aria-hidden="true">
                          <span class={step} data-state={run.round > 1 ? 'done' : 'active'} />
                          <span
                            class={step}
                            data-state={
                              run.status === 'complete' ? 'done' : run.round > 1 ? 'active' : undefined
                            }
                          />
                        </div>
                        <span>{run.branch}</span>
                      </div>

                      <Show when={run.detail}>
                        <span class={fieldHint}>{run.detail}</span>
                      </Show>

                      <div class={row}>
                        <Show when={run.status === 'running'}>
                          <button
                            type="button"
                            class={action}
                            onClick={() => setRunStatus(automation.id, run.id, 'cancelled')}
                          >
                            <X size={11} />
                            Cancel run
                          </button>
                        </Show>
                        <Show when={run.status === 'waiting'}>
                          <button
                            type="button"
                            class={action}
                            onClick={() => setRunStatus(automation.id, run.id, 'running')}
                          >
                            <Check size={11} />
                            Approve
                          </button>
                          <button
                            type="button"
                            class={action}
                            data-muted=""
                            onClick={() => setRunStatus(automation.id, run.id, 'cancelled')}
                          >
                            <X size={11} />
                            Reject
                          </button>
                          <span class={fieldHint}>Resolve this wait (webhook or button).</span>
                        </Show>
                        <Show when={run.status === 'complete'}>
                          <button
                            type="button"
                            class={action}
                            data-muted={cleanup() === run.id ? undefined : ''}
                            onClick={() => setCleanup(cleanup() === run.id ? null : run.id)}
                          >
                            Branches to clean up
                          </button>
                          <Show when={cleanup() === run.id}>
                            <button type="button" class={action} data-danger="">
                              Delete branch
                            </button>
                            <button
                              type="button"
                              class={action}
                              data-muted=""
                              onClick={() => setCleanup(null)}
                            >
                              Keep
                            </button>
                          </Show>
                        </Show>
                        <Show when={run.status === 'cancelled'}>
                          <button
                            type="button"
                            class={action}
                            data-muted=""
                            onClick={() => setRunStatus(automation.id, run.id, 'running')}
                          >
                            <Play size={11} />
                            Continue
                          </button>
                        </Show>
                      </div>
                    </div>
                  )}
                </For>
              </Show>

              <div class={field}>
                <span class={fieldLabel}>Choose the tag this automation manages</span>
                <div class={row}>
                  <For each={['work', 'release', 'life admin', 'home']}>
                    {(tag) => (
                      <button
                        type="button"
                        class={chip}
                        data-active={automation.boundTags.includes(tag) ? '' : undefined}
                        onClick={() =>
                          patch(automation.id, (entry) => ({
                            ...entry,
                            boundTags: entry.boundTags.includes(tag)
                              ? entry.boundTags.filter((name) => name !== tag)
                              : [...entry.boundTags, tag],
                          }))
                        }
                      >
                        #{tag}
                      </button>
                    )}
                  </For>
                </div>
              </div>

              <div class={rowBetween}>
                <span class={fieldHint}>Completed runs keep their history for 30 days.</span>
                <button type="button" class={action} data-muted="">
                  Cancel this run and hide its steps
                </button>
              </div>
            </Show>
          </article>
        )}
      </For>
    </ViewShell>
  )
}
