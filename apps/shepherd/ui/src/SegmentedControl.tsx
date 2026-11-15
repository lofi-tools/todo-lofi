import { For } from 'solid-js'

export interface SegmentedChoice<Value extends string> {
  value: Value
  label: string
}

/**
 * A tab-like control for a few exclusive choices. The track stays weak and the
 * selected option carries the strong background, so the group reads as one unit.
 */
export function SegmentedControl<Value extends string>(props: {
  label: string
  value: Value
  choices: readonly SegmentedChoice<Value>[]
  onChange: (value: Value) => void
}) {
  return (
    <div class="segmented" role="group" aria-label={props.label}>
      <For each={props.choices}>
        {(choice) => (
          <button type="button" class="segmented__option"
            aria-pressed={props.value === choice.value}
            onClick={() => props.onChange(choice.value)}>{choice.label}</button>
        )}
      </For>
    </div>
  )
}
