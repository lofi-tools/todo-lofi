import { Show } from 'solid-js'
import { LevelPicker } from './LevelPicker'
import { PickerCaret, PickerMenu, type PickerOption } from './PickerMenu'
import type { AgentModelSelection, CodexModel } from './bridge'

export interface ModelChoice {
  id: string
  name: string
  efforts?: string[]
  defaultEffort?: string | null
  description?: string | null
}

export function codexModelChoices(models: CodexModel[]): ModelChoice[] {
  return models.map((model) => ({
    id: model.model,
    name: model.displayName,
    efforts: model.supportedReasoningEfforts.map((effort) => effort.reasoningEffort),
    defaultEffort: model.defaultReasoningEffort,
  }))
}

export function ModelPicker(props: {
  label: string
  emptyLabel: string
  triggerClass: string
  model?: string | null
  effort?: string | null
  models: ModelChoice[]
  displayNames?: boolean
  allowCustom?: boolean
  allowDefault?: boolean
  showEffort?: boolean
  above?: boolean
  align?: 'start' | 'end'
  within?: string
  openRequest?: number
  disabled?: boolean
  onChange: (selection: AgentModelSelection) => void
}) {
  const effortLabel = (effort: string) => effort.charAt(0).toUpperCase() + effort.slice(1)
  const id = (model: string | null) => JSON.stringify(model)
  const triggerText = () => {
    const displayName = props.displayNames ? props.models.find((model) => model.id === props.model)?.name : undefined
    const model = displayName ?? (props.model || props.emptyLabel)
    return props.showEffort && props.effort ? `${model} · ${effortLabel(props.effort)}` : model
  }

  const selectedModel = () => props.models.find((model) => model.id === props.model)
  const levels = () => [...new Set(selectedModel()?.efforts ?? [])]
    .sort((left, right) => effortRank(left) - effortRank(right))
    .map((effort) => ({ id: effort, label: effortLabel(effort) }))
  const currentLevel = () => {
    const preferred = props.effort ?? selectedModel()?.defaultEffort
    return levels().find((level) => level.id === preferred) ?? levels()[0]
  }
  function catalogOption(model: ModelChoice): PickerOption {
    return {
      id: id(model.id),
      name: props.displayNames ? model.name : model.id,
      detail: props.displayNames ? [model.id, model.description].filter(Boolean).join(' · ') : model.name,
      description: props.displayNames || model.name === model.id ? undefined : model.name,
      keepOpen: !!model.efforts?.length,
    }
  }

  const options = (): PickerOption[] => [
    ...(props.allowDefault ? [{ id: id(null), name: 'Agent default' }] : []),
    ...(props.model && !props.models.some((model) => model.id === props.model)
      ? [{ id: id(props.model), name: props.model, disabled: true }] : []),
    ...props.models.map(catalogOption),
  ]

  return <PickerMenu label={props.label} text={triggerText()} value={id(props.model || null)}
    triggerClass={props.triggerClass} options={options()} filterable above={props.above} align={props.align} within={props.within}
    disabled={props.disabled} openRequest={props.openRequest}
    customOption={props.allowCustom ? (query) => {
      if (query.length > 512 || query === props.model || props.models.some((model) => model.id === query)) return
      return { id: id(query), name: `Use model ID “${query}”` }
    } : undefined}
    footer={props.models.some((model) => model.efforts?.length) ? <Show when={currentLevel()}>{(level) =>
      <span class="model-picker__effort">
        <span class="model-picker__effort-model">{selectedModel()?.name || props.model}</span>
        <span class="model-picker__effort-label">
          <span>{level().label}</span>
          <Show when={props.showEffort && props.effort}>
            <button type="button" class="model-picker__effort-reset"
              onClick={() => props.onChange({ model: props.model, effort: null })}>Use model default</button>
          </Show>
        </span>
        <LevelPicker label="Reasoning effort" levels={levels()} value={level().id} disabled={props.disabled}
          onChange={(effort) => props.onChange({ model: props.model, effort })} />
      </span>
    }</Show> : undefined}
    onChange={(selection) => props.onChange({ model: JSON.parse(selection) as string | null, effort: null })}>
    <span class="model-picker__value">{triggerText()}</span>
    <PickerCaret />
  </PickerMenu>
}


function effortRank(effort: string) {
  switch (effort.toLowerCase().replace(/[\s_-]/g, '')) {
    case 'off': case 'none': return 0
    case 'minimal': return 1
    case 'low': return 2
    case 'medium': return 3
    case 'high': return 4
    case 'xhigh': case 'extrahigh': case 'veryhigh': return 5
    case 'max': case 'maximum': return 6
    default: return 7
  }
}
