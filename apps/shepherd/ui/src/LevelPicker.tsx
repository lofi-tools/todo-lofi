import { For } from 'solid-js'

export function LevelPicker(props: {
  label: string
  levels: readonly { id: string; label: string }[]
  value: string
  disabled?: boolean
  onChange: (value: string) => void
}) {
  const index = () => Math.max(0, props.levels.findIndex((level) => level.id === props.value))
  const last = () => Math.max(0, props.levels.length - 1)
  const position = (stop: number) => last() ? `${stop / last() * 100}%` : '50%'

  return <span class="level-picker">
    <span class="level-picker__rail" aria-hidden="true" style={{ '--level-position': position(index()) }}>
      <span class="level-picker__fill" />
      <For each={props.levels}>{(_, stop) => <span class="level-picker__stop"
        classList={{ 'level-picker__stop--active': stop() <= index() }}
        style={{ left: position(stop()) }} />}</For>
      <span class="level-picker__thumb" />
    </span>
    <input type="range" class="level-picker__input" aria-label={props.label}
      aria-valuetext={props.levels[index()]?.label} min={0} max={last()} step={1}
      value={index()} disabled={props.disabled || last() === 0} onInput={(event) => {
        const level = props.levels[event.currentTarget.valueAsNumber]
        if (level) props.onChange(level.id)
      }} />
  </span>
}
