import { For, Show, createEffect, createResource, createSignal, on, type JSX } from 'solid-js'

import { AGENT_LABELS, projectApprovals, resetApprovalGrant, type CommandBinding, type CommandRegistry, type KeybindingEdit, type ProjectApproval, type ProjectSnapshot, type SessionApproval, type SettingsSnapshot, type SettingsEdit, type SettingKey } from './bridge'
import { displayChord, normalizeChord } from './keybindings'

const categories = ['Workspace', 'Projects', 'Appearance', 'Keyboard shortcuts'] as const

export const SEND_ACCENTS = ['Blue', 'Violet', 'Teal', 'Amber', 'Neutral'] as const
export type SendAccent = (typeof SEND_ACCENTS)[number]

interface SettingsPaneProps {
  preferences?: SettingsSnapshot
  preferencesEditable: boolean
  preferenceBusy: boolean
  onEditPreference: (key: SettingKey, value: SettingsEdit['value'], projectId?: string | null) => Promise<void>
  onReloadPreferences: () => void
  settingsConflicts: Omit<SettingsEdit, 'previous'>[]
  onRetryPreference: (edit: Omit<SettingsEdit, 'previous'>) => void
  onDiscardConflict: (edit: Omit<SettingsEdit, 'previous'>) => void
  keybindings?: CommandRegistry
  mac: boolean
  onReloadKeybindings: () => Promise<CommandRegistry>
  onEditKeybinding: (edit: KeybindingEdit) => Promise<void>
  showArchived: boolean
  onShowArchived: (value: boolean) => void
  notificationsEnabled: boolean
  onNotificationsEnabled: (enabled: boolean) => void
  sendAccent: SendAccent
  onSendAccent: (accent: SendAccent) => Promise<void>
  onAddProject: () => void
  pending: boolean
  projectImports: JSX.Element
  project?: ProjectSnapshot
  projectSettingsRequest?: { id: string }
  approvalChange?: { projectId: string }
  showProjectArchived: boolean
  onShowProjectArchived: (projectId: string, visible: boolean | null) => void
  onSavePreviewUrl: (projectId: string, url: string) => Promise<void>
}

function ProjectPreviewSetting(props: { url?: string | null; onSave: (url: string) => Promise<void> }) {
  const [url, setUrl] = createSignal(props.url ?? '')
  const [saving, setSaving] = createSignal(false)
  const [error, setError] = createSignal('')
  createEffect(on(() => props.url, (saved) => setUrl(saved ?? '')))

  async function save(event: SubmitEvent) {
    event.preventDefault()
    if (saving()) return
    setSaving(true)
    setError('')
    try {
      await props.onSave(url())
      setUrl(props.url ?? '')
    } catch (failure) {
      setError(settingsError(failure))
    } finally {
      setSaving(false)
    }
  }

  return (
    <form class="settings__project" onSubmit={save}>
      <label class="settings__preview-url">
        Preview URL
        <input type="text" inputmode="url" value={url()} onInput={(event) => setUrl(event.currentTarget.value)}
          disabled={saving()} placeholder="http://localhost:3000" />
      </label>
      <button type="submit" class="project-action" disabled={saving() || url() === (props.url ?? '')}>Save preview URL</button>
      <Show when={error()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
    </form>
  )
}

function ApprovalGrantList(props: {
  title: string
  empty: string
  grants?: (ProjectApproval | SessionApproval)[]
  resetting: boolean
  onReset: (id: string, taskId?: string) => void
}) {
  return (
    <section class="settings__project" aria-label={props.title}>
      <h4>{props.title}</h4>
      <For each={props.grants} fallback={<p class="empty">{props.empty}</p>}>
        {(grant) => {
          const task = 'taskId' in grant ? grant : undefined
          return (
            <div class="settings__approval">
              <div>
                <p>{grant.title}</p>
                <p class="empty">{AGENT_LABELS[grant.agent]}{task && ` · ${task.taskTitle}`}</p>
                <Show when={grant.paths.length > 0}>
                  <p class="entry__permission-paths">{grant.paths.join(', ')}</p>
                </Show>
                <Show when={grant.arguments != null}>
                  <pre class="entry__arguments">{typeof grant.arguments === 'string' ? grant.arguments : JSON.stringify(grant.arguments, null, 2)}</pre>
                </Show>
              </div>
              <button
                type="button"
                class="project-action"
                disabled={props.resetting}
                aria-label={`Reset approval for ${grant.title}${task ? ` in ${task.taskTitle}` : ''}`}
                onClick={() => props.onReset(grant.id, task?.taskId)}
              >
                Reset
              </button>
            </div>
          )
        }}
      </For>
    </section>
  )
}

function ProjectApprovals(props: { projectId: string; change?: { projectId: string } }) {
  const [resetting, setResetting] = createSignal(false)
  const [resetError, setResetError] = createSignal('')
  const [approvals, { refetch }] = createResource(() => props.projectId, async (projectId) => {
    try {
      return { snapshot: await projectApprovals(projectId), error: '' }
    } catch (error) {
      return { snapshot: undefined, error: settingsError(error) }
    }
  })

  createEffect(on(() => props.change, (change) => {
    if (change?.projectId === props.projectId) void refetch()
  }, { defer: true }))

  async function reset(approvalId: string, taskId?: string) {
    setResetting(true)
    setResetError('')
    try {
      await resetApprovalGrant(props.projectId, approvalId, taskId)
      await refetch()
    } catch (error) {
      setResetError(settingsError(error))
    } finally {
      setResetting(false)
    }
  }

  return (
    <div class="settings__project">
      <Show when={resetError() || approvals.latest?.error}>
        {(error) => <p class="entry__notice--error" role="alert">{error()}</p>}
      </Show>
      <Show when={approvals.latest?.snapshot}>
        <ApprovalGrantList
          title="Saved project approvals"
          empty="No saved project approvals."
          grants={approvals.latest?.snapshot?.saved}
          resetting={resetting()}
          onReset={reset}
        />
        <ApprovalGrantList
          title="Live session approvals"
          empty="No live session approvals."
          grants={approvals.latest?.snapshot?.sessions}
          resetting={resetting()}
          onReset={reset}
        />
      </Show>
    </div>
  )
}

function settingsError(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'message' in error) return String(error.message)
  return String(error)
}

function NumberPreference(props: {
  label: string
  value: number
  inherited: number
  overridden: boolean
  project: boolean
  minimum: number
  maximum: number
  editable: boolean
  onSave: (value: number | null) => Promise<void>
}) {
  const [draft, setDraft] = createSignal(String(props.value))
  const [error, setError] = createSignal('')
  createEffect(on(() => props.value, (value, previous) => {
    if (previous === undefined || draft() === String(previous)) setDraft(String(value))
    if (draft() === String(value)) setError('')
  }))
  const number = () => draft().trim() ? Number(draft()) : NaN
  const valid = () => Number.isSafeInteger(number()) && number() >= props.minimum && number() <= props.maximum

  async function save(value: number | null) {
    setError('')
    try {
      await props.onSave(value)
      setDraft(String(props.value))
    } catch (failure) {
      setError(settingsError(failure))
    }
  }

  return (
    <form class="settings__project" onSubmit={(event) => {
      event.preventDefault()
      if (props.editable && valid()) void save(number())
    }}>
      <label class="settings__preview-url">
        {props.label}
        <input type="number" min={props.minimum} max={props.maximum} step="1" value={draft()}
          disabled={!props.editable} onInput={(event) => setDraft(event.currentTarget.value)} />
      </label>
      <p class="empty">{props.project ? 'Workspace value' : 'Built-in default'} {props.inherited}</p>
      <div class="settings__shortcut-row">
        <button type="submit" class="project-action"
          disabled={!props.editable || !valid() || number() === props.value}>Save {props.label.toLowerCase()}</button>
        <Show when={props.overridden}>
          <button type="button" class="project-action" disabled={!props.editable}
            onClick={() => void save(null)}>Reset {props.label.toLowerCase()}</button>
        </Show>
      </div>
      <Show when={error()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
    </form>
  )
}

const numberPreferences = [
  { key: 'attachments.max_file_bytes', label: 'Attachment limit (bytes)', minimum: 1, maximum: Number.MAX_SAFE_INTEGER },
  { key: 'terminal.scrollback_lines', label: 'Terminal scrollback (lines)', minimum: 0, maximum: 100_000 },
] as const

function NumberPreferences(props: {
  preferences: SettingsSnapshot
  project?: ProjectSnapshot
  editable: boolean
  onEdit: SettingsPaneProps['onEditPreference']
}) {
  return (
    <For each={numberPreferences}>
      {({ key, label, minimum, maximum }) => {
        const scope = () => props.project ? props.preferences.projects[props.project.path] : props.preferences.app
        const inherited = () => (props.project ? props.preferences.app[key] : undefined) ?? props.preferences.defaults[key]
        return <NumberPreference label={label} minimum={minimum} maximum={maximum} editable={props.editable}
          value={scope()?.[key] ?? inherited()} inherited={inherited()} overridden={scope()?.[key] !== undefined}
          project={!!props.project} onSave={(value) => props.onEdit(key, value, props.project?.id)} />
      }}
    </For>
  )
}

function KeyboardShortcuts(props: {
  registry?: CommandRegistry
  mac: boolean
  onReload: () => Promise<CommandRegistry>
  onEdit: (edit: KeybindingEdit) => Promise<void>
}) {
  const [editing, setEditing] = createSignal<{ id: string; previous: string | null }>()
  const [chord, setChord] = createSignal<string | null>(null)
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal('')
  const [status, setStatus] = createSignal('')
  let opener: HTMLButtonElement | undefined
  let recorder: HTMLInputElement | undefined

  const commands = () => props.registry?.commands ?? []
  const editable = () => !!props.registry && !props.registry.warning && !pending()
  const previous = (command: CommandBinding) => command.overridden ? command.chord : null
  const format = (chord: string | null | undefined) => chord ? displayChord(chord, props.mac) : 'Unassigned'

  function begin(command: CommandBinding, button: HTMLButtonElement) {
    opener = button
    setError('')
    setStatus('')
    setChord(null)
    setEditing({ id: command.id, previous: previous(command) })
    recorder?.focus()
  }

  function finish() {
    setEditing(undefined)
    setChord(null)
    opener?.focus()
    opener = undefined
  }

  function record(event: KeyboardEvent) {
    if (event.isComposing || event.keyCode === 229) return
    const unmodified = !event.ctrlKey && !event.altKey && !event.metaKey
    if (unmodified && event.key === 'Tab') return
    event.preventDefault()
    if (event.repeat) return
    if (unmodified && event.key === 'Escape') {
      setError('')
      finish()
      return
    }
    const next = normalizeChord(event)
    if (next) setChord(next)
  }

  async function apply(edit: KeybindingEdit) {
    if (!editable()) return
    setPending(true)
    setError('')
    setStatus('')
    try {
      await props.onEdit(edit)
      setStatus(edit.chord ? 'Shortcut saved.' : 'Default shortcut restored.')
      setPending(false)
      finish()
    } catch (failure) {
      setError(settingsError(failure))
    } finally {
      setPending(false)
    }
  }

  async function reload() {
    if (pending()) return
    setPending(true)
    setError('')
    setStatus('')
    try {
      const registry = await props.onReload()
      const active = editing()
      const command = registry.commands.find(({ id }) => id === active?.id)
      if (command) setEditing({ id: command.id, previous: previous(command) })
      if (!registry.warning) setStatus('Shortcuts reloaded.')
    } catch (failure) {
      setError(settingsError(failure))
    } finally {
      setPending(false)
    }
  }

  return (
    <div class="settings__project">
      <div class="settings__shortcut-row">
        <p class="empty">Record one chord per command.</p>
        <button class="project-action" type="button" disabled={pending()} onClick={() => void reload()}>Reload configuration</button>
      </div>
      <Show when={props.registry?.warning}>
        <p class="empty">Repair the keybindings file, then reload configuration to edit shortcuts.</p>
      </Show>
      <Show when={error()}>{(message) => <p class="entry__notice--error" role="alert">{message()}</p>}</Show>
      <Show when={status()}>{(message) => <p class="empty" role="status">{message()}</p>}</Show>
      <ul class="settings__shortcut-list" aria-label="Command shortcuts">
        <For each={commands().map(({ id }) => id)}>
          {(id) => {
            const command = () => commands().find((command) => command.id === id)
            let editButton: HTMLButtonElement | undefined
            return <Show when={command()}>{(binding) => (
              <li class="settings__project">
                <div class="settings__shortcut-row">
                  <div class="settings__shortcut-label">
                    <span>{binding().label}</span>
                    <span class="empty">{binding().category}</span>
                  </div>
                  <span>
                    <kbd>{format(binding().chord)}</kbd>
                    <Show when={binding().overridden}><small class="empty">Default {format(binding().defaultChord)}</small></Show>
                  </span>
                  <button ref={editButton} type="button" class="project-action" disabled={!editable() || !!editing()}
                    aria-label={`Edit ${binding().label} shortcut`} onClick={(event) => begin(binding(), event.currentTarget)}>Edit</button>
                  <Show when={binding().overridden}>
                    <button type="button" class="project-action" disabled={!editable() || !!editing()}
                      aria-label={`Reset ${binding().label} shortcut`} onClick={() => {
                        opener = editButton
                        void apply({ command: id, previous: previous(binding()), chord: null })
                      }}>Reset</button>
                  </Show>
                </div>
                <Show when={editing()?.id === id}>
                  <form class="settings__shortcut-row" onSubmit={(event) => {
                    event.preventDefault()
                    const edit = editing()
                    if (edit && chord()) void apply({ command: edit.id, previous: edit.previous, chord: chord() })
                  }}>
                    <input data-shortcut-capture ref={recorder} aria-label={`Record shortcut for ${binding().label}`}
                      placeholder="Press a shortcut" value={chord() ? format(chord()) : ''} readonly disabled={pending()} onKeyDown={record} />
                    <button type="submit" class="project-action" disabled={!editable() || !chord()}>Save shortcut</button>
                    <button type="button" class="project-action" disabled={pending()} onClick={() => { setError(''); finish() }}>Cancel</button>
                  </form>
                </Show>
              </li>
            )}</Show>
          }}
        </For>
      </ul>
    </div>
  )
}

export function SettingsPane(props: SettingsPaneProps) {
  const [category, setCategory] = createSignal<(typeof categories)[number]>('Workspace')
  createEffect(on(() => props.projectSettingsRequest, (request) => {
    if (request) setCategory('Projects')
  }))

  return (
    <section class="settings" aria-labelledby="settings-title">
      <header class="pane-header">
        <h2 id="settings-title">Settings</h2>
      </header>
      <div class="settings__body">
        <nav class="settings__nav" aria-label="Settings categories">
          <For each={categories}>
            {(name) => (
              <button
                type="button"
                class="nav-item"
                aria-current={category() === name ? 'true' : undefined}
                onClick={() => setCategory(name)}
              >
                {name}
              </button>
            )}
          </For>
        </nav>
        <section class="settings__category" aria-labelledby="settings-category-title">
          <h3 id="settings-category-title">{category()}</h3>
          <Show when={props.settingsConflicts.length}>
            <div class="settings__project">
              <p class="empty">Reload configuration to review the changed value, then retry or discard your edit.</p>
              <button type="button" class="project-action" disabled={props.preferenceBusy} onClick={props.onReloadPreferences}>Reload settings</button>
              <For each={props.settingsConflicts}>{(edit) => (
              <div class="settings__shortcut-row">
                <span class="empty">{edit.key}</span>
                <button type="button" class="project-action" disabled={!props.preferencesEditable} onClick={() => props.onRetryPreference(edit)}>Retry setting change</button>
                <button type="button" class="project-action" disabled={props.preferenceBusy} onClick={() => props.onDiscardConflict(edit)}>Discard setting change</button>
              </div>
              )}</For>
            </div>
          </Show>
          <Show when={category() === 'Workspace'}>
            <label>
              <input
                type="checkbox"
                checked={props.showArchived}
                disabled={!props.preferencesEditable}
                onChange={(event) => {
                  const visible = event.currentTarget.checked
                  event.currentTarget.checked = props.showArchived
                  props.onShowArchived(visible)
                }}
              />
              Show archived tasks
            </label>
            <label>
              <input type="checkbox" checked={props.notificationsEnabled} disabled={!props.preferencesEditable}
                onChange={(event) => {
                  const enabled = event.currentTarget.checked
                  event.currentTarget.checked = props.notificationsEnabled
                  props.onNotificationsEnabled(enabled)
                }} />
              Notify when tasks need input
            </label>
            <Show when={props.preferences}>
              {(snapshot) => <NumberPreferences preferences={snapshot()} editable={props.preferencesEditable} onEdit={props.onEditPreference} />}
            </Show>
            <Show when={!props.settingsConflicts.length}>
              <button type="button" class="project-action" disabled={props.preferenceBusy} onClick={props.onReloadPreferences}>Reload settings</button>
            </Show>
          </Show>
          <Show when={category() === 'Projects'}>
            <Show when={props.project}>
              {(project) => (
                <div class="settings__project">
                  <h4>Project settings</h4>
                  <p>{project().name}</p>
                  <p class="empty settings__project-path">{project().path}</p>
                  <label>
                    <input
                      type="checkbox"
                      checked={props.showProjectArchived}
                      disabled={!props.preferencesEditable}
                      onChange={(event) => {
                        const visible = event.currentTarget.checked
                        event.currentTarget.checked = props.showProjectArchived
                        props.onShowProjectArchived(project().id, visible)
                      }}
                    />
                    Show archived tasks
                  </label>
                  <Show when={props.preferences?.projects[project().path]?.['tasks.show_archived'] !== undefined}>
                    <button type="button" class="project-action" disabled={!props.preferencesEditable}
                      onClick={() => props.onShowProjectArchived(project().id, null)}>Use workspace archive visibility</button>
                  </Show>
                  <Show when={project().id} keyed>
                    {(projectId) => <>
                      <ProjectPreviewSetting url={project().previewUrl} onSave={(url) => props.onSavePreviewUrl(projectId, url)} />
                      <Show when={props.preferences}>
                        {(snapshot) => <NumberPreferences preferences={snapshot()} project={project()}
                          editable={props.preferencesEditable} onEdit={props.onEditPreference} />}
                      </Show>
                      <ProjectApprovals projectId={projectId} change={props.approvalChange} />
                    </>}
                  </Show>
                </div>
              )}
            </Show>
            <button type="button" class="project-action" onClick={props.onAddProject} disabled={props.pending}>Add project</button>
            <p class="empty">Import all from</p>
            {props.projectImports}
          </Show>
          <Show when={category() === 'Appearance'}>
            <fieldset class="settings__accent-picker">
              <legend>Send button accent</legend>
              <For each={SEND_ACCENTS}>
                {(accent) => (
                  <input
                    class="settings__accent-swatch"
                    type="radio"
                    name="send-accent"
                    data-send-accent={accent}
                    aria-label={accent}
                    title={accent}
                    checked={props.sendAccent === accent}
                    disabled={!props.preferencesEditable}
                    onChange={(event) => {
                      const group = event.currentTarget.closest('fieldset')
                      void props.onSendAccent(accent).then(() => {
                        const saved = group?.querySelector<HTMLInputElement>(`input[data-send-accent="${props.sendAccent}"]`)
                        if (saved) saved.checked = true
                      })
                    }}
                  />
                )}
              </For>
            </fieldset>
          </Show>
          <Show when={category() === 'Keyboard shortcuts'}>
            <KeyboardShortcuts registry={props.keybindings} mac={props.mac}
              onReload={props.onReloadKeybindings} onEdit={props.onEditKeybinding} />
          </Show>
        </section>
      </div>
    </section>
  )
}
