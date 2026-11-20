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
  const id = (model: string | null, effort?: string | null) => JSON.stringify([model, effort])
  const triggerText = () => {
    const displayName = props.displayNames ? props.models.find((model) => model.id === props.model)?.name : undefined
    const model = displayName ?? (props.model || props.emptyLabel)
    return props.showEffort && props.effort ? `${model} · ${effortLabel(props.effort)}` : model
  }

  function catalogOption(model: ModelChoice): PickerOption {
    const effort = props.model === model.id ? props.effort ?? model.defaultEffort : undefined
    const choices = (model.efforts ?? []).map((level) => ({ id: id(model.id, level), name: effortLabel(level) }))
    if (props.showEffort && choices.length) {
      choices.unshift({ id: id(model.id, null), name: 'Model default' })
    }
    return {
      id: id(model.id),
      name: props.displayNames ? model.name : model.id,
      detail: props.displayNames ? [model.id, model.description].filter(Boolean).join(' · ') : model.name,
      description: props.displayNames || model.name === model.id ? undefined : model.name,
      submenu: choices.length ? {
        label: 'Reasoning effort',
        value: props.model === model.id ? id(model.id, props.showEffort ? props.effort ?? null : effort) : undefined,
        options: choices,
      } : undefined,
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
    onChange={(selection) => {
      const [model, effort] = JSON.parse(selection) as [string | null, string | null]
      props.onChange({ model, effort: effort ?? null })
    }}>
    <span class="model-picker__value">{triggerText()}</span>
    <PickerCaret />
  </PickerMenu>
}
