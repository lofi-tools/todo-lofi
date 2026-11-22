import { Index, Show, createEffect, createResource, createSignal, on } from 'solid-js'

import {
  AGENT_LABELS, antigravityProjectOptions, codexProjectModels, opencodeProjectModels,
  type AgentModelFallback, type AgentModelPreferences, type AgentModelSelection, type ModelAgent,
} from './bridge'
import { createSettingsDraft, SettingsSave } from './SettingsDraft'
import { SettingsSelect } from './SettingsSelect'
import { ModelPicker, codexModelChoices, type ModelChoice } from './ModelPicker'

const modelAgents: ModelAgent[] = ['codex', 'opencode', 'pi', 'claude', 'antigravity']

export function supportsModelSelection(agent: string): agent is ModelAgent {
  return modelAgents.some((candidate) => candidate === agent)
}

async function loadCatalog(projectId: string, agent: ModelAgent): Promise<ModelChoice[]> {
  if (agent === 'codex') return codexModelChoices((await codexProjectModels(projectId)).models)
  const models = agent === 'opencode' ? await opencodeProjectModels(projectId)
    : agent === 'antigravity' ? (await antigravityProjectOptions(projectId)).models
    : undefined
  return (models?.options ?? []).map((model) => ({ id: model.id, name: model.name, efforts: [] }))
}

function ModelFields(props: {
  projectId?: string
  agent: ModelAgent
  value: AgentModelSelection
  modelLabel: string
  allowDefault?: boolean
  disabled: boolean
  onChange: (value: AgentModelSelection) => void
}) {
  const [catalog] = createResource(
    () => props.projectId && ['codex', 'opencode', 'antigravity'].includes(props.agent)
      ? { projectId: props.projectId, agent: props.agent } : false,
    async ({ projectId, agent }) => {
      try { return { projectId, agent, models: await loadCatalog(projectId, agent) } }
      catch (failure) {
        console.warn('Could not load the model catalog for settings', failure)
        return { projectId, agent, models: [] }
      }
    },
  )
  const models = () => {
    const result = catalog.latest
    return result?.projectId === props.projectId && result?.agent === props.agent ? result.models : []
  }
  return <div class="settings__row">
    <span>{props.modelLabel}</span>
    <ModelPicker label={props.modelLabel} emptyLabel={props.allowDefault ? 'Agent default' : 'Choose model'}
      triggerClass="settings-select settings-select--picker" model={props.value.model} effort={props.value.effort}
      models={models()} allowCustom showEffort allowDefault={props.allowDefault} disabled={props.disabled} onChange={props.onChange} />
  </div>
}

export function AgentModelSettings(props: {
  agent: ModelAgent
  projectId?: string
  saved?: AgentModelPreferences
  editable: boolean
  onSave: (value: AgentModelPreferences) => Promise<void>
}) {
  const savedSelection = () => ({ model: props.saved?.model ?? null, effort: props.saved?.effort ?? null })
  const [defaults, setDefaults] = createSignal<AgentModelSelection>(savedSelection())
  const [fallbacks, setFallbacks] = createSignal<AgentModelFallback[]>(props.saved?.fallbacks ?? [])
  const saved = () => JSON.stringify([savedSelection(), props.saved?.fallbacks ?? []])
  const serialized = () => JSON.stringify([defaults(), fallbacks()])
  function reset() {
    setDefaults(savedSelection())
    setFallbacks(props.saved?.fallbacks ?? [])
  }
  createEffect(on(saved, (_, previous) => { if (serialized() === previous) reset() }))
  const draft = createSettingsDraft({
    dirty: () => serialized() !== saved(),
    editable: () => props.editable,
    discard: reset,
    save,
  })
  const disabled = () => draft.saving() || !props.editable
  const agentOptions = modelAgents.map((agent) => ({ value: agent, label: AGENT_LABELS[agent] }))

  function updateFallback(index: number, patch: Partial<AgentModelFallback>) {
    setFallbacks((entries) => entries.map((entry, position) => position === index ? { ...entry, ...patch } : entry))
  }

  function moveFallback(index: number, direction: number) {
    setFallbacks((entries) => {
      const entry = entries[index]
      const destination = index + direction
      if (!entry || destination < 0 || destination >= entries.length) return entries
      const next = entries.filter((_, position) => position !== index)
      next.splice(destination, 0, entry)
      return next
    })
  }

  async function save() {
    const entries = fallbacks().map((entry) => ({
      agent: entry.agent, model: entry.model.trim(), effort: entry.effort?.trim() || null,
    }))
    if (entries.some((entry) => !entry.model)) {
      throw new Error('Enter a model ID for every fallback.')
    }
    const defaultModel = defaults().model?.trim() || null
    const piModels = entries.filter((entry) => entry.agent === 'pi').map((entry) => entry.model)
    if (props.agent === 'pi' && defaultModel) piModels.push(defaultModel)
    if (piModels.some((model) => !/^[^/]+\/.+/.test(model))) {
      throw new Error('Enter Pi models as provider/model-id.')
    }
    await props.onSave({
      model: defaultModel,
      effort: defaults().effort?.trim() || null,
      fallbacks: entries,
    })
    reset()
  }

  return <section class="settings__agent-models" aria-label="Model settings" data-settings-draft onKeyDown={draft.onKeyDown}>
    <ModelFields projectId={props.projectId} agent={props.agent} value={defaults()} modelLabel="Default model"
      allowDefault disabled={disabled()} onChange={(patch) => setDefaults((current) => ({ ...current, ...patch }))} />
    <Show when={props.agent !== 'codex'}><p class="empty">This integration does not expose reasoning effort in Shepherd.</p></Show>
    <h4>Fallback chain</h4>
    <ol class="settings__fallbacks">
      <Index each={fallbacks()}>{(entry, index) => <li class="settings__fallback">
        <div class="settings__row"><span>Agent</span>
          <SettingsSelect label={`Fallback ${index + 1} agent`} value={entry().agent} options={agentOptions}
            disabled={disabled()} onChange={(input) => {
              if (supportsModelSelection(input.value)) updateFallback(index, { agent: input.value, model: '', effort: null })
            }} />
        </div>
        <ModelFields projectId={props.projectId} agent={entry().agent} value={entry()} modelLabel={`Fallback ${index + 1} model`}
          disabled={disabled()}
          onChange={(patch) => updateFallback(index, { ...patch, model: patch.model ?? entry().model })} />
        <div class="settings__fallback-actions">
          <button type="button" class="project-action" aria-label={`Move fallback ${index + 1} up`} disabled={disabled() || index === 0} onClick={() => moveFallback(index, -1)}>Move up</button>
          <button type="button" class="project-action" aria-label={`Move fallback ${index + 1} down`} disabled={disabled() || index + 1 >= fallbacks().length} onClick={() => moveFallback(index, 1)}>Move down</button>
          <button type="button" class="project-action" aria-label={`Remove fallback ${index + 1}`} disabled={disabled()} onClick={() => setFallbacks((entries) => entries.filter((_, position) => position !== index))}>Remove</button>
        </div>
      </li>}</Index>
    </ol>
    <div class="settings__fallback-actions">
      <button type="button" class="project-action" disabled={disabled()} onClick={() => setFallbacks((entries) => [...entries, { agent: props.agent, model: '' }])}>Add fallback</button>
    </div>
    <SettingsSave draft={draft} label="Save model settings" />
  </section>
}
