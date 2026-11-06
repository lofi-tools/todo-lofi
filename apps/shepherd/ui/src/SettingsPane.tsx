import { For, Show, createEffect, createResource, createSignal, on, type JSX } from 'solid-js'

import { AGENT_LABELS, projectApprovals, resetApprovalGrant, type ProjectApproval, type ProjectSnapshot, type SessionApproval } from './bridge'

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
  approvalChange?: { projectId: string }
  showProjectArchived: boolean
  onShowProjectArchived: (projectId: string, visible: boolean) => void
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

function ProjectApprovals(props: { projectId: string; change?: { projectId: string } }) {
  const [resetting, setResetting] = createSignal(false)
  const [resetError, setResetError] = createSignal('')
  const [approvals, { refetch }] = createResource(() => props.projectId, async (projectId) => {
    try {
      return { snapshot: await projectApprovals(projectId), error: '' }
    } catch (error) {
      return { snapshot: undefined, error: approvalError(error) }
    }
  })

  createEffect(on(() => props.change, (change) => {
    if (change?.projectId === props.projectId) void refetch()
  }, { defer: true }))

  async function reset(approvalId: string, taskId?: string) {
    setResetting(true)
    setResetError('')
    try {
      await resetApprovalGrant(props.projectId, approvalId, taskId)
      await refetch()
    } catch (error) {
      setResetError(approvalError(error))
    } finally {
      setResetting(false)
    }
  }

  return (
    <div class="settings__project">
      <Show when={resetError() || approvals.latest?.error}>
        {(error) => <p class="entry__notice--error" role="alert">{error()}</p>}
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

function approvalError(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'message' in error) return String(error.message)
  return String(error)
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
                  <Show when={project().id} keyed>
                    {(projectId) => <ProjectApprovals projectId={projectId} change={props.approvalChange} />}
                  </Show>
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
