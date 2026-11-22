import { createSettingsDraft, SettingsSave } from './SettingsDraft'
import { For, Index, Show, createEffect, createMemo, createResource, createSignal, on, type JSX } from 'solid-js'

import { AGENT_LABELS, harnessVersionStatus, projectApprovals, resetApprovalGrant, type PoolAgent, type CommandBinding, type CommandRegistry, type KeybindingEdit, type ProjectApproval, type ProjectSnapshot, type PreviewService, type SessionApproval, type SettingsSnapshot, type SettingsEdit, type SettingKey } from './bridge'
import { displayChord, normalizeChord } from './keybindings'
import { reportError } from './diagnostics'
import { HarnessWarning, PoolSettings, type HarnessDiscovery } from './PoolSettings'
import { SettingsSelect } from './SettingsSelect'
import { AgentModelSettings, supportsModelSelection } from './AgentModelSettings'
import { AgentIcon } from './AgentIcon'
import { actOnWorkspaceFile, projectEnvironment, saveProjectEnvironment, type ProjectEnvironment, type WorktreeSetup } from './bridge'

export const categories = ['Workspace', 'Projects', 'Agents', 'Appearance', 'Keyboard shortcuts'] as const

function matchesSearch(text: string, query: string) {
  return !query || query.split(/\s+/).every((word) => text.toLowerCase().includes(word))
}

export const SEND_ACCENTS = ['Blue', 'Violet', 'Teal', 'Amber', 'Neutral'] as const
export type SendAccent = (typeof SEND_ACCENTS)[number]

export type SettingsCategory = (typeof categories)[number]

interface SettingsPaneProps {
  category: SettingsCategory
  onCategoryChange: (category: SettingsCategory) => void
  settingsProjectId?: string
  onSettingsProjectChange: (projectId?: string) => void
  onBack: () => void
  onNavigate: (action: () => void) => void
  discovery: HarnessDiscovery
  agentAvailability?: Record<import('./bridge').Agent, boolean>
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
  projects: ProjectSnapshot[]
  activeProjectId?: string | null
  projectSettingsRequest?: { id: string }
  approvalChange?: { projectId?: string }
  showProjectArchived: (projectId: string) => boolean
  onShowProjectArchived: (projectId: string, visible: boolean | null) => void
}

function createSimpleEdit<Value>(value: () => string, onSave: (value: Value) => Promise<void>) {
  const [draft, setDraft] = createSignal(value())
  const [saving, setSaving] = createSignal(false)
  const [error, setError] = createSignal('')
  let attempted: Value | undefined
  createEffect(on(value, (saved, previous) => {
    if (draft() === previous) setDraft(saved)
    if (draft() === saved) setError('')
  }))
  function edit(value: string) {
    attempted = undefined
    setDraft(value)
    setError('')
  }
  async function save(next: Value, retry = false) {
    if (saving() || (!retry && attempted === next)) return
    attempted = next
    setSaving(true)
    setError('')
    try {
      await onSave(next)
      setDraft(value())
    } catch (failure) {
      setError(settingsError(failure))
    } finally {
      setSaving(false)
    }
  }
  return { draft, edit, save, saving, error }
}

function ProjectPreviewSetting(props: { url?: string | null; localOverride: boolean; editable: boolean; onSave: (url: string) => Promise<void> }) {
  const saved = () => props.url ?? ''
  const field = createSimpleEdit(saved, props.onSave)
  function save(retry = false) {
    if (props.editable && !props.localOverride && field.draft().trim() !== saved()) void field.save(field.draft().trim(), retry)
  }

  return (
    <form class="settings__project" onSubmit={(event) => { event.preventDefault(); save(true) }}>
      <label class="settings__row settings__preview-url">
        <span>Preview URL</span>
        <input type="text" inputmode="url" value={field.draft()} onInput={(event) => field.edit(event.currentTarget.value)}
          onBlur={() => save()} disabled={!props.editable} readOnly={field.saving() || props.localOverride} placeholder="http://localhost:3000" />
      </label>
      <Show when={props.localOverride}><p class="empty">Set in .config/shepherd/settings.json.</p></Show>
      <Show when={field.error()}><p role="alert">{field.error()}</p></Show>
    </form>
  )
}

function ProjectPreviewServers(props: { value: PreviewService[]; localOverride: boolean; editable: boolean; onSave: (value: PreviewService[] | null) => Promise<void> }) {
  const [servers, setServers] = createSignal<PreviewService[]>(structuredClone(props.value))
  const saved = () => JSON.stringify(props.value)
  const serialized = () => JSON.stringify(servers())
  const reset = () => setServers(structuredClone(props.value))
  createEffect(on(saved, (_, previous) => { if (serialized() === previous) reset() }))
  const draft = createSettingsDraft({
    dirty: () => !props.localOverride && serialized() !== saved(),
    editable: () => props.editable && !props.localOverride,
    discard: reset,
    async save() {
      if (!servers().every((server) => server.name.trim() && server.command.trim() &&
        (!server.portVariable || /^(PORT|[A-Z][A-Z0-9_]*_PORT)$/.test(server.portVariable)))) {
        throw new Error('Enter a name and command for every server. Port variables must be PORT or end in _PORT.')
      }
      await props.onSave(servers().length ? servers() : null)
      reset()
    },
  })
  const disabled = () => draft.saving() || !draft.editable()
  function update(index: number, patch: Partial<PreviewService>) {
    setServers((current) => current.map((server, position) => position === index ? { ...server, ...patch } : server))
  }
  return <section class="settings__project" aria-label="Preview servers and background commands" data-settings-draft onKeyDown={draft.onKeyDown}>
    <h4>Preview servers and background commands</h4>
    <Index each={servers()}>{(server, index) => <div class="settings__project">
      <label class="settings__row"><span>Name</span><input aria-label={`Server ${index + 1} name`} value={server().name} disabled={disabled()} onInput={(event) => update(index, { name: event.currentTarget.value })} /></label>
      <label class="settings__row"><span>Command</span><input aria-label={`Server ${index + 1} command`} value={server().command} disabled={disabled()} onInput={(event) => update(index, { command: event.currentTarget.value })} /></label>
      <label class="settings__row"><span>Port variable</span><input aria-label={`Server ${index + 1} port variable`} value={server().portVariable ?? ''} disabled={disabled()} onInput={(event) => update(index, { portVariable: event.currentTarget.value || null })} placeholder="PORT" /></label>
      <button type="button" class="project-action" disabled={disabled()} onClick={() => setServers((current) => current.filter((_, position) => position !== index))}>Remove</button>
    </div>}</Index>
    <Show when={props.localOverride}><p class="empty">Set in .config/shepherd/settings.json. Edit that file to change it.</p></Show>
    <button type="button" class="project-action" disabled={disabled()} onClick={() => setServers((current) => [...current, { name: '', command: '', portVariable: null }])}>Add server</button>
    <Show when={!props.localOverride}><SettingsSave draft={draft} label="Save servers" /></Show>
  </section>
}

function WorktreeSetupSettings(props: {
  projectId: string
  value?: WorktreeSetup | null
  localOverride: boolean
  editable: boolean
  onEdit: SettingsPaneProps['onEditPreference']
}) {
  const formatted = () => JSON.stringify(props.value ?? {}, null, 2)
  const [configuration, setConfiguration] = createSignal(formatted())
  createEffect(on(formatted, (value, previous) => {
    if (configuration() === previous) setConfiguration(value)
  }))
  const draft = createSettingsDraft({
    dirty: () => !props.localOverride && configuration() !== formatted(),
    editable: () => props.editable && !props.localOverride,
    discard: () => setConfiguration(formatted()),
    async save() {
      const value: unknown = JSON.parse(configuration())
      if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Enter a JSON object for worktree commands.')
      await props.onEdit('worktree.setup', value as WorktreeSetup, props.projectId)
      setConfiguration(formatted())
    },
  })

  return <section class="settings__project" aria-label="Worktree commands" data-settings-draft onKeyDown={draft.onKeyDown}>
    <h4>Worktree commands</h4>
    <label class="settings__row"><span>Setup configuration</span>
      <textarea value={configuration()} rows={8} readOnly={!draft.editable() || draft.saving()}
        onInput={(event) => setConfiguration(event.currentTarget.value)} />
    </label>
    <p class="empty">{props.localOverride ? 'Set in .config/shepherd/settings.json.' :
      'Use afterCheckout, readinessCheck, launchWrapper, and setupTerminal. Commands are arrays of arguments.'}</p>
    <Show when={!props.localOverride}><SettingsSave draft={draft} label="Save worktree commands" /></Show>
  </section>
}

function ProjectEnvironmentSettings(props: { projectId: string; useImportedSetup?: boolean; localOverride: boolean; editable: boolean }) {
  const [snapshot, { mutate, refetch }] = createResource(() => props.projectId, projectEnvironment)
  const [draft, setDraft] = createSignal<ProjectEnvironment>()
  const [error, setError] = createSignal('')
  const [updating, setUpdating] = createSignal(false)
  const saved = () => snapshot.latest?.current
  function reset() {
    const environment = saved()
    setDraft(environment ? structuredClone(environment) : undefined)
  }
  createEffect(on(saved, (value) => setDraft(value ? structuredClone(value) : undefined)))
  const section = createSettingsDraft({
    dirty: () => !props.localOverride && !!draft() && JSON.stringify(draft()) !== JSON.stringify(saved()),
    editable: () => props.editable && !props.localOverride && !updating(),
    discard: reset,
    async save() {
      const environment = draft()
      if (environment) mutate(await saveProjectEnvironment(props.projectId, saved() ?? null, environment))
    },
  })
  const disabled = () => !section.editable() || section.saving()

  async function save(environment: ProjectEnvironment) {
    const current = snapshot.latest?.current
    if (disabled() || section.dirty()) return
    setUpdating(true)
    setError('')
    try { mutate(await saveProjectEnvironment(props.projectId, current ?? null, environment)) }
    catch (failure) { setError(settingsError(failure)); reportError('project_environment', failure) }
    finally { setUpdating(false) }
  }

  return <section class="settings__project" aria-label="Project environment" data-settings-draft onKeyDown={section.onKeyDown}>
    <h4>Worktree setup and project actions</h4>
    <Show when={snapshot.error}><p class="empty">Could not load the project environment: {settingsError(snapshot.error)}</p><button class="project-action" onClick={() => void refetch()}>Retry</button></Show>
    <Show when={error()}><p role="alert">{error()}</p></Show>
    <Show when={snapshot.latest?.warning}>{(warning) => <p role="alert">Could not read the Codex environment: {warning()}</p>}</Show>
    <Show when={!props.localOverride && snapshot.latest?.source && saved() && snapshot.latest.source.sourceHash !== saved()?.sourceHash}>
      <p>The Codex environment changed. Updating replaces your Shepherd copy.</p>
      <button class="project-action" disabled={disabled() || section.dirty()} onClick={() => {
        const source = snapshot.latest?.source
        if (source) void save(source)
      }}>Update from Codex</button>
    </Show>
    <Show when={draft()} fallback={<><p class="empty">No Codex environment found in this project.</p><Show when={!props.localOverride}><button class="project-action" disabled={disabled() || snapshot.loading} onClick={() => void save({ sourceHash: '', setup: '', actions: [] })}>Create project environment</button></Show></>}>
      {(environment) => <>
        <label class="settings__row"><span>Worktree setup script</span><textarea value={environment().setup} readOnly={disabled()} onInput={(event) => setDraft({ ...environment(), setup: event.currentTarget.value })} /></label>
        <Show when={environment().setup.trim()}>
          <p class="empty">{props.useImportedSetup === false
            ? 'This project uses its configured worktree setup.'
            : 'Runs once in each new task worktree before the agent starts.'}</p>
        </Show>
        <Index each={environment().actions}>{(action, index) => <div class="settings__project">
          <label class="settings__row"><span>Action name</span><input value={action().name} readOnly={disabled()} onInput={(event) => setDraft({ ...environment(), actions: environment().actions.map((item, position) => position === index ? { ...item, name: event.currentTarget.value } : item) })} /></label>
          <label class="settings__row"><span>Action command</span><input value={action().command} readOnly={disabled()} onInput={(event) => setDraft({ ...environment(), actions: environment().actions.map((item, position) => position === index ? { ...item, command: event.currentTarget.value } : item) })} /></label>
          <Show when={!props.localOverride}><button class="project-action" disabled={disabled()} onClick={() => setDraft({ ...environment(), actions: environment().actions.filter((_, position) => position !== index) })}>Remove action</button></Show>
        </div>}</Index>
        <Show when={!props.localOverride} fallback={<p class="empty">Set in .config/shepherd/settings.json.</p>}>
          <button class="project-action" disabled={disabled()} onClick={() => setDraft({ ...environment(), actions: [...environment().actions, { name: '', icon: 'run', command: '' }] })}>Add action</button>
          <SettingsSave draft={section} label="Save environment" />
        </Show>
      </>}
    </Show>
  </section>
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

function ProjectApprovals(props: { projectId: string; change?: { projectId?: string } }) {
  const [resetting, setResetting] = createSignal(false)
  const [approvals, { refetch }] = createResource(() => props.projectId, async (projectId) => {
    try {
      return { snapshot: await projectApprovals(projectId), error: '' }
    } catch (error) {
      reportError('project_approvals', error)
      return { snapshot: undefined, error: settingsError(error) }
    }
  })

  createEffect(on(() => props.change, (change) => {
    if (change && (!change.projectId || change.projectId === props.projectId)) void refetch()
  }, { defer: true }))

  async function reset(approvalId: string, taskId?: string) {
    setResetting(true)
    try {
      await resetApprovalGrant(props.projectId, approvalId, taskId)
      await refetch()
    } catch (error) {
      reportError('reset_approval_grant', error)
    } finally {
      setResetting(false)
    }
  }

  return (
    <div class="settings__project">
      <Show when={approvals.latest?.error}>
        <button type="button" class="project-action" onClick={() => void refetch()}>Retry approvals</button>
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
  localOverride: boolean
  value: number
  inherited: number
  overridden: boolean
  project: boolean
  minimum: number
  maximum: number
  editable: boolean
  onSave: (value: number | null) => Promise<void>
}) {
  const field = createSimpleEdit(() => String(props.value), props.onSave)
  const number = () => field.draft().trim() ? Number(field.draft()) : NaN
  function save(retry = false) {
    const value = number()
    if (props.editable && !props.localOverride && Number.isSafeInteger(value) &&
      value >= props.minimum && value <= props.maximum && value !== props.value) void field.save(value, retry)
  }

  return (
    <form class="settings__project" onSubmit={(event) => { event.preventDefault(); save(true) }}>
      <label class="settings__row settings__preview-url">
        <span>{props.label}</span>
        <input type="number" min={props.minimum} max={props.maximum} step="1" value={field.draft()}
          disabled={!props.editable} readOnly={props.localOverride || field.saving()}
          onInput={(event) => field.edit(event.currentTarget.value)} onBlur={() => save()} />
      </label>
      <p class="empty">{props.localOverride ? 'Set in .config/shepherd/settings.json.' :
        `${props.project ? 'Workspace value' : 'Built-in default'} ${props.inherited}`}</p>
      <Show when={field.error()}><p role="alert">{field.error()}</p></Show>
      <Show when={props.overridden && !props.localOverride}>
        <button type="button" class="project-action" disabled={!props.editable || field.saving()}
          onClick={() => void field.save(null, true)}>Reset {props.label.toLowerCase()}</button>
      </Show>
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
  query?: string
}) {
  return (
    <For each={numberPreferences.filter(({ key, label }) => matchesSearch(`${props.project ? 'projects' : 'workspace'} ${key} ${label}`, props.query ?? ''))}>
      {({ key, label, minimum, maximum }) => {
        const scope = () => props.project ? props.preferences.projects[props.project.path] : props.preferences.app
        const inherited = () => (props.project ? props.preferences.app[key] : undefined) ?? props.preferences.defaults[key]
        return <NumberPreference label={label} localOverride={props.project ? Object.hasOwn(props.preferences.projectLocal?.[props.project.path] ?? {}, key) : false} minimum={minimum} maximum={maximum} editable={props.editable}
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
  query?: string
}) {
  const [editing, setEditing] = createSignal<{ id: string; previous: string | null }>()
  const [chord, setChord] = createSignal<string | null>(null)
  const [pending, setPending] = createSignal(false)
  function setError(message: string) {
    if (message) reportError('keyboard_shortcuts', message)
  }
  const [status, setStatus] = createSignal('')
  let opener: HTMLButtonElement | undefined
  let recorder: HTMLInputElement | undefined

  const commands = () => (props.registry?.commands ?? []).filter((command) => matchesSearch(`keyboard shortcuts ${command.label} ${command.id} ${command.category} ${command.chord ?? ''}`, props.query ?? ''))
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
                  <span hidden={editing()?.id === id}>
                    <kbd>{format(binding().chord)}</kbd>
                    <Show when={binding().overridden}><small class="empty">Default {format(binding().defaultChord)}</small></Show>
                  </span>
                  <button ref={editButton} type="button" class="project-action" hidden={editing()?.id === id} disabled={!editable() || !!editing()}
                    aria-label={`Edit ${binding().label} shortcut`} onClick={(event) => begin(binding(), event.currentTarget)}>Edit</button>
                  <Show when={binding().overridden && editing()?.id !== id}>
                    <button type="button" class="project-action" disabled={!editable() || !!editing()}
                      aria-label={`Reset ${binding().label} shortcut`} onClick={() => {
                        opener = editButton
                        void apply({ command: id, previous: previous(binding()), chord: null })
                      }}>Reset</button>
                  </Show>
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
                </div>
              </li>
            )}</Show>
          }}
        </For>
      </ul>
    </div>
  )
}

function HarnessSettings(props: { agent: PoolAgent; discovery: HarnessDiscovery }) {
  const harnesses = () => (props.discovery.snapshot()?.harnesses ?? []).filter((entry) => entry.agent === props.agent)
  const issue = () => props.discovery.snapshot()?.errors[props.agent]
  return (
    <section class="settings__harnesses" aria-label="Detected harnesses" aria-busy={props.discovery.checking()}>
      <p class="empty">Harnesses found in configured paths, on PATH, in common Nix profiles, or in supported app locations. Restart Shepherd after changing harness settings.</p>
      <Show when={issue()}>{(message) => <p role="alert">{message()}</p>}</Show>
      <Show when={!props.discovery.checking() && !props.discovery.error() && !issue() && !harnesses().length}><p class="empty">No harnesses detected.</p></Show>
      <For each={harnesses()}>{(harness) => (
        <div class="settings__harness">
          <div class="settings__harness-name"><AgentIcon agent={harness.agent} />{AGENT_LABELS[harness.agent]}{harness.path.includes('/ChatGPT.app/') ? ' · ChatGPT app' : harness.path.includes('/Codex.app/') ? ' · Codex app' : harness.agent === 'antigravity' ? (harness.path.endsWith('.app') ? ' · app' : ' · CLI') : ''}</div>
          <code>{harness.path}</code>
          <span class="empty">{harnessVersionStatus(harness.agent, harness.version, harness.path)}</span>
          <Show when={harness.issue}>{(warning) => <HarnessWarning issue={warning()} />}</Show>
          <Show when={harness.issue && harness.lastVerifiedAt}>{(checkedAt) =>
            <span class="empty">Last verified {new Date(checkedAt() * 1000).toLocaleString()}</span>
          }</Show>
        </div>
      )}</For>
    </section>
  )
}

export function SettingsPane(props: SettingsPaneProps) {
  const category = () => props.category
  const setCategory = props.onCategoryChange
  const settingsProjectId = () => props.settingsProjectId
  const setSettingsProjectId = props.onSettingsProjectChange
  const settingsProject = createMemo(() => props.projects.find((project) => project.id === settingsProjectId()))
  const [search, setSearch] = createSignal('')
  const [selectedHarness, setSelectedHarness] = createSignal<PoolAgent>()
  const selectedModelAgent = () => {
    const agent = selectedHarness()
    return agent && supportsModelSelection(agent) ? agent : undefined
  }
  const query = () => search().trim().toLowerCase()
  const searchTerms: Record<(typeof categories)[number], string> = {
    Workspace: 'show archived tasks notify notifications when tasks need input attachment limit bytes terminal scrollback lines reload settings configuration',
    Projects: 'project settings show archived tasks preview url server background command port attachment limit bytes terminal scrollback lines approvals saved live session add project import chatgpt worktree setup project actions codex environment',
    Agents: 'default model effort fallback chain agent pool priority installed agents reorder harnesses codex chatgpt opencode pi claude code freebuff antigravity kilocode executable path refresh',
    Appearance: 'appearance theme system light dark density compact comfortable send button accent blue violet teal amber neutral',
    'Keyboard shortcuts': 'keyboard shortcuts shortcut bindings commands reload configuration',
  }
  const matchingCategories = createMemo(() => categories.filter((name) => {
    const value = query()
    if (!value) return true
    if (matchesSearch(`${name} ${searchTerms[name]}`, value)) return true
    return name === 'Keyboard shortcuts' && (props.keybindings?.commands ?? []).some((command) =>
      matchesSearch(`keyboard shortcuts ${command.label} ${command.id} ${command.category} ${command.chord ?? ''}`, value))
  }))
  const activeCategory = () => matchingCategories().includes(category()) ? category() : matchingCategories()[0]
  createEffect(on(activeCategory, (section) => {
    if (section === 'Agents') void props.discovery.refresh()
  }))
  const showSetting = (description: string) => matchesSearch(`${activeCategory() ?? ''} ${description}`, query()) || matchesSearch(activeCategory() ?? '', query())
  const narrowedQuery = () => matchesSearch(activeCategory() ?? '', query()) ? '' : query()
  const local = () => props.preferences?.projectLocal?.[settingsProject()?.path ?? ''] ?? {}
  const repoManaged = (key: SettingKey) => Object.hasOwn(local(), key)
  const repoConfig = '.config/shepherd/settings.json'

  function chooseHarness(agent?: PoolAgent) {
    if (agent !== selectedHarness()) props.onNavigate(() => setSelectedHarness(agent))
  }

  const theme = () => props.preferences?.app.theme ?? 'system'
  const density = () => props.preferences?.app['appearance.density'] ?? 'compact'

  async function chooseAppearance(key: 'theme' | 'appearance.density', input: HTMLSelectElement) {
    const value = input.value
    if (value !== 'system' && value !== 'light' && value !== 'dark' && value !== 'compact' && value !== 'comfortable') return
    try {
      await props.onEditPreference(key, value)
    } catch (failure) {
      reportError('save_appearance', failure)
    } finally {
      input.value = key === 'theme' ? theme() : density()
    }
  }
  createEffect(on(() => props.projectSettingsRequest, (request) => {
    if (request) setSearch('')
  }))

  return (
    <section class="settings" aria-labelledby="settings-title">
      <div class="settings__body" classList={{ 'settings__body--detail': activeCategory() === 'Agents' && !!selectedHarness() }}>
        <nav class="settings__nav" aria-label="Settings categories">
          <button type="button" class="settings__back" onClick={props.onBack}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M19 12H5m7-7-7 7 7 7" /></svg>Back to app</button>
          <header class="settings__header"><h2 id="settings-title">Settings</h2></header>
          <div class="settings__search">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="10.5" cy="10.5" r="6.5" /><path d="m15.5 15.5 5 5" /></svg>
            <input class="task-search__input" type="search" aria-label="Search settings" placeholder="Search settings" value={search()} onInput={(event) => {
              const input = event.currentTarget
              const value = input.value
              props.onNavigate(() => setSearch(value))
              if (value !== search()) input.value = search()
            }} />
          </div>
          <For each={matchingCategories()}>
            {(name) => (
              <>
                <button
                  type="button"
                  class="nav-item"
                  aria-current={activeCategory() === name && (name !== 'Projects' || !settingsProjectId()) ? 'true' : undefined}
                  onClick={() => {
                    if (name === category() && (name !== 'Projects' || !settingsProjectId())) return
                    props.onNavigate(() => {
                      if (name === 'Projects') setSettingsProjectId(undefined)
                      setCategory(name)
                    })
                  }}
                >
                  {name}
                </button>
                <Show when={name === 'Projects' && activeCategory() === 'Projects' && props.projects.length > 0}>
                  <div class="settings__project-nav" role="group" aria-label="Project settings">
                    <For each={props.projects}>{(project) => (
                      <button type="button" class="nav-item settings__project-link" title={project.path}
                        aria-current={settingsProjectId() === project.id ? 'true' : undefined}
                        onClick={() => { if (project.id !== settingsProjectId()) props.onNavigate(() => setSettingsProjectId(project.id)) }}>{project.name}</button>
                    )}</For>
                  </div>
                </Show>
              </>
            )}
          </For>
        </nav>
        <section class="settings__category" aria-labelledby="settings-category-title">
          <h3 id="settings-category-title">{activeCategory() ?? 'No matching settings'}</h3>
          <Show when={!activeCategory()}><p class="empty">Try another search.</p></Show>
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
          <Show when={activeCategory() === 'Workspace'}>
            <Show when={showSetting('show archived tasks')}><label class="settings__row settings__row--checkbox"><span>Show archived tasks</span>
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
            </label></Show>
            <Show when={showSetting('notify when tasks need input notifications')}><label class="settings__row settings__row--checkbox"><span>Notify when tasks need input</span>
              <input type="checkbox" checked={props.notificationsEnabled} disabled={!props.preferencesEditable}
                onChange={(event) => {
                  const enabled = event.currentTarget.checked
                  event.currentTarget.checked = props.notificationsEnabled
                  props.onNotificationsEnabled(enabled)
                }} />
            </label></Show>
            <Show when={props.preferences}>
              {(snapshot) => <NumberPreferences preferences={snapshot()} editable={props.preferencesEditable} onEdit={props.onEditPreference} query={narrowedQuery()} />}
            </Show>
            <Show when={!props.settingsConflicts.length && showSetting('reload settings configuration')}>
              <button type="button" class="project-action" disabled={props.preferenceBusy} onClick={props.onReloadPreferences}>Reload settings</button>
            </Show>
          </Show>
          <Show when={activeCategory() === 'Projects'}>
            <Show when={settingsProject()}>
              {(project) => (
                <div class="settings__project">
                  <h4>Project settings</h4>
                  <p>{project().name}</p>
                  <p class="empty settings__project-path">{project().path}</p>
                  <Show when={Object.keys(local()).length > 0}>
                    <div class="settings__save">
                      <button type="button" class="project-action" onClick={() =>
                        void actOnWorkspaceFile(project().id, null, repoConfig, 'edit').catch((failure) => reportError('open_project_config', failure))
                      }>Open config file</button>
                      <code>{repoConfig}</code>
                    </div>
                  </Show>
                  <Show when={showSetting('show archived tasks')}><label class="settings__row settings__row--checkbox"><span>Show archived tasks</span>
                    <input
                      type="checkbox"
                      checked={props.showProjectArchived(project().id)}
                      disabled={!props.preferencesEditable || repoManaged('tasks.show_archived')}
                      onChange={(event) => {
                        const visible = event.currentTarget.checked
                        event.currentTarget.checked = props.showProjectArchived(project().id)
                        props.onShowProjectArchived(project().id, visible)
                      }}
                    />
                  </label>
                    <Show when={repoManaged('tasks.show_archived')}><p class="empty">Set in {repoConfig}.</p></Show>
                  </Show>
                  <Show when={showSetting('show archived tasks') && !repoManaged('tasks.show_archived') && props.preferences?.projects[project().path]?.['tasks.show_archived'] !== undefined}>
                    <button type="button" class="project-action" disabled={!props.preferencesEditable}
                      onClick={() => props.onShowProjectArchived(project().id, null)}>Use workspace archive visibility</button>
                  </Show>
                  <Show when={project().id} keyed>
                    {(projectId) => <>
                      <Show when={showSetting('preview url')}><ProjectPreviewSetting url={repoManaged('preview.url') ? local()['preview.url'] : props.preferences?.projects[project().path]?.['preview.url'] ?? project().previewUrl}
                        localOverride={repoManaged('preview.url')} editable={props.preferencesEditable}
                        onSave={(url) => props.onEditPreference('preview.url', url.trim() || null, projectId)} /></Show>
                      <Show when={showSetting('preview server command background')}><ProjectPreviewServers
                        value={props.preferences?.projects[project().path]?.['preview.servers'] ?? []}
                        localOverride={repoManaged('preview.servers')}
                        editable={props.preferencesEditable}
                        onSave={(value) => props.onEditPreference('preview.servers', value, projectId)} /></Show>
                      <Show when={showSetting('worktree setup launch wrapper environment commands')}>
                        <WorktreeSetupSettings projectId={projectId}
                          value={props.preferences?.projects[project().path]?.['worktree.setup']}
                          localOverride={repoManaged('worktree.setup')}
                          editable={props.preferencesEditable} onEdit={props.onEditPreference} />
                      </Show>
                      <Show when={showSetting('worktree setup project actions codex environment')}><ProjectEnvironmentSettings projectId={projectId}
                        localOverride={repoManaged('environment')} editable={props.preferencesEditable}
                        useImportedSetup={props.preferences?.projects[project().path]?.['worktree.setup']?.useImportedSetup} /></Show>
                      <Show when={props.preferences}>
                        {(snapshot) => <NumberPreferences preferences={snapshot()} project={project()}
                          editable={props.preferencesEditable} onEdit={props.onEditPreference} query={narrowedQuery()} />}
                      </Show>
                      <Show when={showSetting('approvals saved project live session')}><ProjectApprovals projectId={projectId} change={props.approvalChange} /></Show>
                    </>}
                  </Show>
                </div>
              )}
            </Show>
            <Show when={!settingsProject()}>
              <Show when={showSetting('add project')}><button type="button" class="project-action" onClick={props.onAddProject} disabled={props.pending}>Add project</button></Show>
              <Show when={showSetting('import projects chatgpt')}><p class="empty">Import all from</p>{props.projectImports}</Show>
              <Show when={!!query() && !showSetting('add project') && !showSetting('import projects chatgpt')}><p class="empty">Select a project to view its settings.</p></Show>
            </Show>
          </Show>
          <Show when={activeCategory() === 'Agents'}>
            <PoolSettings discovery={props.discovery} availability={props.agentAvailability} priority={props.preferences?.app['agents.priority']} selected={selectedHarness()} onSelect={(agent) => chooseHarness(selectedHarness() === agent ? undefined : agent)} editable={props.preferencesEditable && !props.preferenceBusy} onChange={(priority) => props.onEditPreference('agents.priority', priority)} />
          </Show>
          <Show when={activeCategory() === 'Appearance'}>
            <Show when={showSetting('theme system light dark')}><label class="settings__row">
              <span id="appearance-theme-label">Theme</span>
              <SettingsSelect label="Theme" value={theme()} disabled={!props.preferencesEditable}
                options={[{ value: 'system', label: 'System' }, { value: 'light', label: 'Light' }, { value: 'dark', label: 'Dark' }]}
                onChange={(input) => void chooseAppearance('theme', input)} />
            </label></Show>
            <Show when={showSetting('density compact comfortable')}><label class="settings__row">
              <span id="appearance-density-label">Density</span>
              <SettingsSelect label="Density" value={density()} disabled={!props.preferencesEditable}
                options={[{ value: 'compact', label: 'Compact' }, { value: 'comfortable', label: 'Comfortable' }]}
                onChange={(input) => void chooseAppearance('appearance.density', input)} />
            </label></Show>
            <Show when={showSetting('send button accent blue violet teal amber neutral')}><div class="settings__row"><span id="send-accent-label">Send button accent</span><fieldset class="settings__accent-picker" aria-labelledby="send-accent-label">
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
            </fieldset></div></Show>
          </Show>
          <Show when={activeCategory() === 'Keyboard shortcuts'}>
            <KeyboardShortcuts registry={props.keybindings} mac={props.mac}
              onReload={props.onReloadKeybindings} onEdit={props.onEditKeybinding}
              query={(props.keybindings?.commands ?? []).some((command) => matchesSearch(`keyboard shortcuts ${command.label} ${command.id} ${command.category} ${command.chord ?? ''}`, query())) ? narrowedQuery() : ''} />
          </Show>
        </section>
        <Show when={activeCategory() === 'Agents' && selectedHarness()}>{(agent) => <aside class="settings__detail" aria-label={`${AGENT_LABELS[agent()]} harness settings`}>
          <header class="settings__detail-header"><h3>{AGENT_LABELS[agent()]} settings</h3><button type="button" class="project-action project-action--icon" aria-label="Close harness settings" title="Close" onClick={() => chooseHarness()}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18" /></svg></button></header>
          <Show when={agent() === 'kilocode'} fallback={<p class="empty">Configure the executable path with <code>agents.{agent()}.executable</code> in settings.json.</p>}><p class="empty">This harness is not yet implemented in Shepherd.</p></Show>
          <Show when={agent() === 'codex'}><p class="empty">Codex environment overrides: <code>agents.codex.environment</code>.</p></Show>
          <Show when={agent() === 'opencode'}><p class="empty">OpenCode configuration file: <code>agents.opencode.config_file</code>.</p></Show>
          <Show when={selectedModelAgent()} keyed>{(modelAgent) =>
            <AgentModelSettings agent={modelAgent}
              projectId={(props.projects.find((project) => project.id === props.activeProjectId && !project.missing)
                ?? props.projects.find((project) => !project.missing))?.id}
              saved={props.preferences?.app['agents.model_preferences']?.[modelAgent]}
              editable={props.preferencesEditable && !props.preferenceBusy}
              onSave={(value) => props.onEditPreference('agents.model_preferences', {
                ...props.preferences?.app['agents.model_preferences'], [modelAgent]: value,
              })} />
          }</Show>
          <Show when={agent() === 'freebuff'}><p class="empty">Freebuff manages its model in its own terminal.</p></Show>
          <HarnessSettings agent={agent()} discovery={props.discovery} />
        </aside>}</Show>
      </div>
    </section>
  )
}
