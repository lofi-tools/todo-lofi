import { For, Show, createSignal } from 'solid-js'
import { css } from 'styled-system/css'
import {
  AlertCircle,
  Check,
  GitHub,
  Link as LinkIcon,
  Repeat,
  Users,
  X,
} from 'web-design-system/icons'
import ViewShell, {
  action,
  card,
  cardHead,
  cardMeta,
  cardTitle,
  chip,
  field,
  fieldHint,
  fieldLabel,
  gridTwo,
  note,
  row,
  rowBetween,
  textInput,
} from './ViewShell'

interface IntegrationState {
  connected: boolean
  enabled: boolean
  account: string | null
  lastSynced: string | null
  autoSync: boolean
  intervalMinutes: number
}

const logo = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '7',
  h: '7',
  flexShrink: '0',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.elevated',
  color: 'fg.muted',
})

const statusRow = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  fontSize: '2xs',
  fontFamily: 'mono',
  color: 'fg.subtle',
})

const intervals = [15, 30, 60, 240, 1440]

/** The integrations page: GitHub and Todoist, each with its own card. */
export default function IntegrationsView() {
  const [github, setGithub] = createSignal<IntegrationState>({
    connected: true,
    enabled: true,
    account: 'nmrshll',
    lastSynced: '4m ago',
    autoSync: true,
    intervalMinutes: 30,
  })
  const [todoist, setTodoist] = createSignal<IntegrationState>({
    connected: true,
    enabled: true,
    account: 'sam@example.com',
    lastSynced: '18m ago',
    autoSync: true,
    intervalMinutes: 60,
  })
  const [token, setToken] = createSignal('')
  const [revealing, setRevealing] = createSignal(false)
  const [copied, setCopied] = createSignal<string | null>(null)

  const deviceCode = 'WDJB-MJHT'

  const patch =
    (setter: typeof setGithub) =>
    (change: Partial<IntegrationState>) =>
      setter((current) => ({ ...current, ...change }))

  const setGithubState = patch(setGithub)
  const setTodoistState = patch(setTodoist)

  const copy = (what: string, value: string) => {
    void navigator.clipboard?.writeText(value).then(
      () => setCopied(what),
      () => setCopied(null),
    )
  }

  return (
    <ViewShell
      title="Integrations"
      subtitle="Connected services, the tags they sync, and when they run"
      actions={
        <span class={statusRow}>
          <Repeat size={11} />
          Background sync on
        </span>
      }
    >
      <div class={gridTwo}>
        <article class={card}>
          <div class={cardHead}>
            <span class={logo}>
              <GitHub size={15} />
            </span>
            <span class={cardTitle}>GitHub</span>
            <span class={cardMeta}>
              <Show when={github().connected} fallback="Not connected">
                Connected as {github().account}
              </Show>
            </span>
            <span class={chip} data-active={github().enabled ? '' : undefined}>
              {github().enabled ? 'Connected' : 'Disabled'}
            </span>
          </div>

          <div class={statusRow}>
            <Show
              when={github().enabled}
              fallback={<span>GitHub is disabled. Expand to re-enable; synced data is kept.</span>}
            >
              <Show when={github().autoSync} fallback={<span>Manual</span>}>
                <span>Automatic sync · every {github().intervalMinutes} min</span>
              </Show>
              <span>· Last synced {github().lastSynced}</span>
            </Show>
          </div>

          <Show
            when={github().connected}
            fallback={
              <>
                <p class={note}>
                  Connect with a personal access token, or use the device flow and enter the code
                  GitHub shows you. It is shown only once.
                </p>
                <div class={field}>
                  <span class={fieldLabel}>Personal access token</span>
                  <span class={fieldHint}>
                    Create one on GitHub: Settings → Developer settings → Personal access tokens
                    (classic). Needs <code>repo</code> to read pull requests.
                  </span>
                  <div class={row}>
                    <input
                      class={textInput}
                      type={revealing() ? 'text' : 'password'}
                      value={token()}
                      placeholder="Paste your token here"
                      aria-label="Personal access token"
                      onInput={(event) => setToken(event.currentTarget.value)}
                    />
                    <button
                      type="button"
                      class={action}
                      data-muted={token().trim() ? undefined : ''}
                      onClick={() => {
                        if (!token().trim()) return
                        setGithubState({ connected: true, account: 'nmrshll' })
                        setToken('')
                      }}
                    >
                      <LinkIcon size={11} />
                      Connect
                    </button>
                  </div>
                </div>
                <div class={field}>
                  <span class={fieldLabel}>Connect with a device code</span>
                  <div class={rowBetween}>
                    <button type="button" class={action} onClick={() => setRevealing((value) => !value)}>
                      {revealing() ? 'Hide code' : 'Requesting a GitHub device code…'}
                    </button>
                    <Show when={revealing()}>
                      <span class={css({ fontFamily: 'mono', fontSize: 'sm', color: 'fg.default' })}>
                        {deviceCode}
                      </span>
                      <button type="button" class={action} onClick={() => copy('device', deviceCode)}>
                        {copied() === 'device' ? 'Code copied' : 'Copy code'}
                      </button>
                    </Show>
                  </div>
                  <span class={fieldHint}>
                    Enter this code on GitHub to finish connecting. The browser tab watches for it.
                  </span>
                </div>
              </>
            }
          >
            <div class={field}>
              <span class={fieldLabel}>Repos</span>
              <ul class={css({ listStyle: 'none', m: '0', p: '0', display: 'flex', flexDirection: 'column', gap: '1.5' })}>
                <li class={statusRow}>
                  <Users size={11} /> todo-list · 3 open pull requests
                </li>
                <li class={statusRow}>
                  <Users size={11} /> recipe app · 1 open pull request
                </li>
              </ul>
              <span class={fieldHint}>No repo bound to the website project.</span>
            </div>

            <div class={field}>
              <span class={fieldLabel}>Automatic syncing</span>
              <span class={fieldHint}>
                <Show
                  when={github().autoSync}
                  fallback="Automatic syncing off. Use Sync now for on-demand passes."
                >
                  How often auto sync runs.
                </Show>
              </span>
              <div class={row}>
                <For each={intervals}>
                  {(minutes) => (
                    <button
                      type="button"
                      class={chip}
                      data-active={github().autoSync && github().intervalMinutes === minutes ? '' : undefined}
                      onClick={() => setGithubState({ autoSync: true, intervalMinutes: minutes })}
                    >
                      {minutes < 60 ? `${minutes} min` : minutes < 1440 ? `${minutes / 60} h` : 'Daily'}
                    </button>
                  )}
                </For>
                <button
                  type="button"
                  class={chip}
                  data-active={!github().autoSync ? '' : undefined}
                  onClick={() => setGithubState({ autoSync: false })}
                >
                  Manual
                </button>
              </div>
            </div>

            <div class={rowBetween}>
              <span class={fieldHint}>
                <Check size={11} /> Personal token saved for @{github().account}
              </span>
              <div class={row}>
                <button type="button" class={action}>
                  Sync now
                </button>
                <button type="button" class={action} data-muted="">
                  Replace the stored token
                </button>
                <button
                  type="button"
                  class={action}
                  data-danger=""
                  onClick={() => setGithubState({ enabled: !github().enabled })}
                >
                  {github().enabled ? 'Disable GitHub' : 'Re-enable GitHub'}
                </button>
              </div>
            </div>
            <span class={fieldHint}>Disable stops syncing but keeps synced data.</span>
          </Show>
        </article>

        <article class={card}>
          <div class={cardHead}>
            <span class={logo}>
              <LinkIcon size={15} />
            </span>
            <span class={cardTitle}>Todoist</span>
            <span class={cardMeta}>Connected as {todoist().account}</span>
            <span class={chip} data-active={todoist().enabled ? '' : undefined}>
              {todoist().enabled ? 'Connected' : 'Disabled'}
            </span>
          </div>

          <div class={statusRow}>
            <Show when={todoist().autoSync} fallback={<span>Manual</span>}>
              <span>Automatic sync · every {todoist().intervalMinutes} min</span>
            </Show>
            <span>· Last synced {todoist().lastSynced}</span>
          </div>

          <Show
            when={todoist().enabled}
            fallback={
              <p class={note}>Disabled. Synced data is kept; re-enable to resume syncing.</p>
            }
          >
            <div class={field}>
              <span class={fieldLabel}>Synced tags</span>
              <div class={row}>
                <span class={chip} data-active="">
                  #inbox · capture on
                </span>
                <span class={chip}>#work</span>
                <span class={chip}>#life admin</span>
              </div>
              <span class={fieldHint}>Bindings are edited on the Apps page.</span>
            </div>

            <div class={field}>
              <span class={fieldLabel}>Automatic syncing</span>
              <div class={row}>
                <For each={intervals}>
                  {(minutes) => (
                    <button
                      type="button"
                      class={chip}
                      data-active={todoist().autoSync && todoist().intervalMinutes === minutes ? '' : undefined}
                      onClick={() => setTodoistState({ autoSync: true, intervalMinutes: minutes })}
                    >
                      {minutes < 60 ? `${minutes} min` : minutes < 1440 ? `${minutes / 60} h` : 'Daily'}
                    </button>
                  )}
                </For>
                <button
                  type="button"
                  class={chip}
                  data-active={!todoist().autoSync ? '' : undefined}
                  onClick={() => setTodoistState({ autoSync: false })}
                >
                  Manual
                </button>
              </div>
            </div>

            <div class={rowBetween}>
              <span class={fieldHint}>
                <AlertCircle size={11} /> No conflicts on the last pass
              </span>
              <div class={row}>
                <button type="button" class={action}>
                  Sync now
                </button>
                <button
                  type="button"
                  class={action}
                  data-danger=""
                  onClick={() => setTodoistState({ enabled: !todoist().enabled })}
                >
                  <Show when={todoist().enabled} fallback="Re-enable Todoist">
                    <X size={11} />
                    Disable Todoist
                  </Show>
                </button>
              </div>
            </div>
          </Show>
        </article>
      </div>
    </ViewShell>
  )
}
