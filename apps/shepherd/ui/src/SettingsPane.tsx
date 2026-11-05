import { For, Show, createSignal } from 'solid-js'

const categories = ['Workspace', 'Projects'] as const

interface SettingsPaneProps {
  showArchived: boolean
  onShowArchived: (value: boolean) => void
  onAddProject: () => void
  pending: boolean
}

export function SettingsPane(props: SettingsPaneProps) {
  const [category, setCategory] = createSignal<(typeof categories)[number]>('Workspace')

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
          <Show when={category() === 'Workspace'}>
            <label>
              <input
                type="checkbox"
                checked={props.showArchived}
                onChange={(event) => props.onShowArchived(event.currentTarget.checked)}
              />
              Show archived tasks
            </label>
          </Show>
          <Show when={category() === 'Projects'}>
            <button type="button" class="project-action" onClick={props.onAddProject} disabled={props.pending}>Add project</button>
          </Show>
        </section>
      </div>
    </section>
  )
}
