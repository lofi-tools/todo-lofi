import { For, Show, createEffect, createSignal, on, type JSX } from 'solid-js'

import type { ProjectSnapshot } from './bridge'

const categories = ['Workspace', 'Projects', 'Appearance'] as const

export const SEND_ACCENTS = ['Blue', 'Violet', 'Teal', 'Amber', 'Neutral'] as const
export type SendAccent = (typeof SEND_ACCENTS)[number]

interface SettingsPaneProps {
  showArchived: boolean
  onShowArchived: (value: boolean) => void
  sendAccent: SendAccent
  onSendAccent: (accent: SendAccent) => void
  onAddProject: () => void
  pending: boolean
  projectImports: JSX.Element
  project?: ProjectSnapshot
  projectSettingsRequest?: { id: string }
  showProjectArchived: boolean
  onShowProjectArchived: (projectId: string, visible: boolean) => void
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
                      onChange={(event) => props.onShowProjectArchived(project().id, event.currentTarget.checked)}
                    />
                    Show archived tasks
                  </label>
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
                    onChange={() => props.onSendAccent(accent)}
                  />
                )}
              </For>
            </fieldset>
          </Show>
        </section>
      </div>
    </section>
  )
}
