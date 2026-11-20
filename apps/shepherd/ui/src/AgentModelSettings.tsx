import { Index, Show, createEffect, createResource, createSignal, on } from 'solid-js'

import {
  AGENT_LABELS, antigravityProjectOptions, codexProjectModels, opencodeProjectModels,
  type AgentModelFallback, type AgentModelPreferences, type AgentModelSelection, type ModelAgent,
} from './bridge'
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
  const [defaults, setDefaults] = createSignal<AgentModelSelection>({})
  const [fallbacks, setFallbacks] = createSignal<AgentModelFallback[]>([])
  const [saving, setSaving] = createSignal(false)
  const [failure, setFailure] = createSignal('')
  createEffect(on(() => [props.agent, props.saved], () => {
    setDefaults({ model: props.saved?.model, effort: props.saved?.effort })
    setFallbacks(props.saved?.fallbacks ?? [])
    setFailure('')
  }))
  const disabled = () => saving() || !props.editable
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

  async function save(event: SubmitEvent) {
    event.preventDefault()
    if (disabled()) return
    const entries = fallbacks().map((entry) => ({
      agent: entry.agent, model: entry.model.trim(), effort: entry.effort?.trim() || null,
    }))
    if (entries.some((entry) => !entry.model)) {
      setFailure('Enter a model ID for every fallback.')
      return
    }
    const defaultModel = defaults().model?.trim() || null
    const piModels = entries.filter((entry) => entry.agent === 'pi').map((entry) => entry.model)
    if (props.agent === 'pi' && defaultModel) piModels.push(defaultModel)
    if (piModels.some((model) => !/^[^/]+\/.+/.test(model))) {
      setFailure('Enter Pi models as provider/model-id.')
      return
    }
    setSaving(true)
    setFailure('')
    try {
      await props.onSave({
        model: defaultModel,
        effort: defaults().effort?.trim() || null,
        fallbacks: entries,
      })
    } catch (error) {
      setFailure(error instanceof Error ? error.message : (error as { message?: string })?.message ?? String(error))
    } finally {
      setSaving(false)
    }
  }

  return <form class="settings__agent-models" onSubmit={(event) => void save(event)}>
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
      <button type="submit" class="project-action" disabled={disabled()}>Save model settings</button>
    </div>
    <Show when={failure()}><p role="alert">{failure()}</p></Show>
  </form>
}
