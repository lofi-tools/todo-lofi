import { For, Show, createSignal } from 'solid-js'
import { css } from 'styled-system/css'
import { Check, ChevronDown, ChevronRight, Link as LinkIcon, Repeat, Settings, Zap } from 'web-design-system/icons'
import { useApp } from '../store'
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
  textInput,
} from './ViewShell'

interface Binding {
  tagId: string
  /** Whether a new task in the tag propagates to the app. */
  captures: boolean
}

interface AppEntry {
  id: string
  label: string
  kind: 'integration' | 'automation'
  description: string
  enabled: boolean
  bindings: Binding[]
}

const description = css({
  fontSize: 'xs',
  color: 'fg.muted',
  lineHeight: 'relaxed',
  maxW: '72ch',
})

const bindingRow = css({
  display: 'flex',
  alignItems: 'center',
  gap: '3',
  px: '3',
  h: '9',
  borderRadius: 'md',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.elevated',
})

const bindingName = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '2',
  flex: '1',
  minW: '0',
  fontSize: 'xs',
  color: 'fg.default',
})

const toggle = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '2',
  fontSize: '2xs',
  color: 'fg.subtle',
  cursor: 'pointer',
  _hover: { color: 'fg.default' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const addRow = css({ display: 'flex', alignItems: 'center', gap: '2' })

const boneyard = css({
  listStyle: 'none',
  display: 'flex',
  flexDirection: 'column',
  gap: '2',
  m: '0',
  p: '0',
})

/**
 * Per-app settings. There is no standalone apps console in the desktop: each
 * app owns the tags it manages, and its card is where those bindings are
 * edited. This page is that list.
 */
export default function AppsView() {
  const app = useApp()
  const [apps, setApps] = createSignal<AppEntry[]>([
    {
      id: 'automations',
      label: 'Coding runs',
      kind: 'automation',
      description:
        'Turns a tagged task into a run: an interview, a plan, then an implementation round on a feature branch.',
      enabled: true,
      bindings: [
        { tagId: 'work', captures: true },
        { tagId: 'release', captures: false },
      ],
    },
    {
      id: 'todoist',
      label: 'Todoist',
      kind: 'integration',
      description:
        'Two-way sync for the tags it manages. New tasks in a captured tag appear in Todoist and come back with their due dates.',
      enabled: true,
      bindings: [{ tagId: 'inbox', captures: true }],
    },
  ])
  const [expanded, setExpanded] = createSignal<string | null>('todoist')
  const [pending, setPending] = createSignal<Record<string, string>>({})

  const tagName = (id: string) => app.tagById(id)?.name ?? id
  const tagColor = (id: string) => app.tagById(id)?.color ?? '#8a8f98'

  const patch = (appId: string, change: (entry: AppEntry) => AppEntry) =>
    setApps((list) => list.map((entry) => (entry.id === appId ? change(entry) : entry)))

  const setEnabled = (appId: string, enabled: boolean) =>
    patch(appId, (entry) => ({ ...entry, enabled }))

  const captureBindings = (appId: string, tagId: string) =>
    patch(appId, (entry) => ({
      ...entry,
      bindings: entry.bindings.map((binding) =>
        binding.tagId === tagId ? { ...binding, captures: !binding.captures } : binding,
      ),
    }))

  const detach = (appId: string, tagId: string) =>
    patch(appId, (entry) => ({
      ...entry,
      bindings: entry.bindings.filter((binding) => binding.tagId !== tagId),
    }))

  const attach = (appId: string) => {
    const tagId = pending()[appId]
    if (!tagId) return
    patch(appId, (entry) =>
      entry.bindings.some((binding) => binding.tagId === tagId)
        ? entry
        : { ...entry, bindings: [...entry.bindings, { tagId, captures: false }] },
    )
    setPending((current) => ({ ...current, [appId]: '' }))
  }

  return (
    <ViewShell
      title="Apps"
      subtitle="Apps that manage content on your behalf, and the tags they are bound to"
    >
      <For each={apps()}>
        {(entry) => {
          const open = () => expanded() === entry.id
          const unattached = () => app.state.tags.filter((tag) => !entry.bindings.some((b) => b.tagId === tag.id))
          return (
            <article class={card}>
              <div class={cardHead}>
                <span class={css({ display: 'inline-flex', color: 'fg.muted' })}>
                  {entry.kind === 'automation' ? <Zap size={14} /> : <LinkIcon size={14} />}
                </span>
                <span class={cardTitle}>{entry.label}</span>
                <span class={chip} data-active={entry.enabled ? '' : undefined}>
                  {entry.enabled ? 'Enabled' : 'Not enabled'}
                </span>
                <span class={cardMeta}>
                  {entry.bindings.length === 0
                    ? 'No tags connected yet'
                    : `Managing ${entry.bindings.length} tag${entry.bindings.length === 1 ? '' : 's'}`}
                </span>
                <button
                  type="button"
                  class={action}
                  data-muted={open() ? undefined : ''}
                  aria-expanded={open()}
                  onClick={() => setExpanded(open() ? null : entry.id)}
                >
                  <Show when={open()} fallback={<ChevronRight size={11} />}>
                    <ChevronDown size={11} />
                  </Show>
                  <Settings size={11} />
                  {open() ? 'Hide settings' : 'Settings'}
                </button>
              </div>

              <Show when={open()}>
                <p class={description}>{entry.description}</p>

                <span class={fieldLabel}>Tags it manages</span>
                <Show
                  when={entry.bindings.length > 0}
                  fallback={<p class={note}>Attach a tag below to start managing its tasks.</p>}
                >
                  <ul class={boneyard}>
                    <For each={entry.bindings}>
                      {(binding) => (
                        <li class={bindingRow}>
                          <span class={bindingName}>
                            <span
                              class={css({ w: '2', h: '2', borderRadius: 'full', flexShrink: '0' })}
                              style={{ background: tagColor(binding.tagId) }}
                            />
                            #{tagName(binding.tagId)}
                          </span>
                          <button
                            type="button"
                            class={toggle}
                            aria-pressed={binding.captures}
                            onClick={() => captureBindings(entry.id, binding.tagId)}
                          >
                            <span class={chip} data-active={binding.captures ? '' : undefined}>
                              <Show when={binding.captures}>
                                <Check size={10} />
                              </Show>
                              New tasks capture
                            </span>
                          </button>
                          <button
                            type="button"
                            class={action}
                            data-muted=""
                            onClick={() => detach(entry.id, binding.tagId)}
                          >
                            Detach
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>

                <div class={field}>
                  <span class={fieldLabel}>Add a tag</span>
                  <span class={fieldHint}>
                    Point {entry.label} at a tag you already keep, then decide what new tasks there do.
                  </span>
                  <div class={addRow}>
                    <select
                      class={textInput}
                      aria-label={`Tag to attach to ${entry.label}`}
                      value={pending()[entry.id] ?? ''}
                      onChange={(event) =>
                        setPending((current) => ({ ...current, [entry.id]: event.currentTarget.value }))
                      }
                    >
                      <option value="">Choose a tag…</option>
                      <For each={unattached()}>
                        {(tag) => <option value={tag.id}>#{tag.name}</option>}
                      </For>
                    </select>
                    <button
                      type="button"
                      class={action}
                      data-muted={pending()[entry.id] ? undefined : ''}
                      onClick={() => attach(entry.id)}
                    >
                      Attach
                    </button>
                  </div>
                </div>

                <Show when={entry.kind === 'automation'}>
                  <div class={rowBetween}>
                    <span class={fieldHint}>
                      <Repeat size={11} /> 1 run active on #{tagName('release')}
                    </span>
                    <button type="button" class={action} data-muted="">
                      Stop runs
                    </button>
                  </div>
                </Show>

                <div class={row}>
                  <button
                    type="button"
                    class={action}
                    onClick={() => setEnabled(entry.id, !entry.enabled)}
                  >
                    {entry.enabled ? `Disable ${entry.label}` : `Re-enable ${entry.label}`}
                  </button>
                  <Show when={!entry.enabled}>
                    <span class={fieldHint}>Synced data is kept; re-enable to resume syncing.</span>
                  </Show>
                </div>
              </Show>
            </article>
          )
        }}
      </For>
    </ViewShell>
  )
}
