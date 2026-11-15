import { For } from 'solid-js'

export function SettingsSelect(props: {
  label: string
  value: string
  options: { value: string; label: string }[]
  disabled?: boolean
  onChange: (input: HTMLSelectElement) => void
}) {
  return (
    <select class="settings-select" aria-label={props.label} value={props.value}
      disabled={props.disabled} onChange={(event) => props.onChange(event.currentTarget)}>
      <For each={props.options}>{(option) => <option value={option.value}>{option.label}</option>}</For>
    </select>
  )
}
