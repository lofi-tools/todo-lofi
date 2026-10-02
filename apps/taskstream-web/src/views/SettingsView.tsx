import { For, Show, createSignal } from 'solid-js'
import { css } from 'styled-system/css'
import { Check, Clock, Folder, Moon, Sun, User } from 'web-design-system/icons'
import ViewShell, {
  action,
  card,
  chip,
  divider,
  field,
  fieldHint,
  fieldLabel,
  row,
  rowBetween,
  textInput,
} from './ViewShell'

type ThemeChoice = 'dark' | 'light' | 'system'

const group = css({ display: 'flex', flexDirection: 'column', gap: '3', maxW: '88ch' })

const groupHead = css({
  display: 'flex',
  alignItems: 'baseline',
  gap: '2',
  pt: '2',
})

const groupTitle = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const settingRow = css({
  display: 'flex',
  alignItems: 'center',
  gap: '4',
  py: '3',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
  '&:last-child': { borderBottomWidth: '0' },
})

const settingBody = css({ display: 'flex', flexDirection: 'column', gap: '0.5', flex: '1', minW: '0' })

const settingLabel = css({ fontSize: 'sm', color: 'fg.default' })

const switchTrack = css({
  display: 'inline-flex',
  alignItems: 'center',
  w: '9',
  h: '5',
  flexShrink: '0',
  p: '0.5',
  borderRadius: 'full',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.strong',
  bg: 'surface.elevated',
  cursor: 'pointer',
  transitionProperty: 'background-color, border-color',
  transitionDuration: 'fast',
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-on]': { bg: 'accent', borderColor: 'accent' },
})

const switchKnob = css({
  w: '3.5',
  h: '3.5',
  borderRadius: 'full',
  bg: 'fg.subtle',
  transitionProperty: 'transform, background-color',
  transitionDuration: 'fast',
  '&[data-on]': { transform: 'translateX(14px)', bg: 'fg.onAccent' },
})

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: 'dark', label: 'Dark' },
  { id: 'light', label: 'Light' },
  { id: 'system', label: 'System' },
]

const INTERVALS = [15, 30, 60, 240, 1440]

/** Read the theme the pre-paint script applied, so the page opens in step. */
function currentTheme(): ThemeChoice {
  const stored = typeof localStorage === 'undefined' ? null : localStorage.getItem('wds-theme')
  return stored === 'light' ? 'light' : stored === 'system' ? 'system' : 'dark'
}

function applyTheme(choice: ThemeChoice) {
  const resolved =
    choice === 'system'
      ? window.matchMedia('(prefers-color-scheme: light)').matches
        ? 'light'
        : 'dark'
      : choice
  document.documentElement.dataset.theme = resolved
  localStorage.setItem('wds-theme', choice)
}

/** The settings page: general, sync, tags & runs, and about. */
export default function SettingsView() {
  const [theme, setTheme] = createSignal<ThemeChoice>(currentTheme())
  const [confirmDelete, setConfirmDelete] = createSignal(true)
  const [showCompleted, setShowCompleted] = createSignal(false)
  const [autoSync, setAutoSync] = createSignal(true)
  const [interval, setInterval] = createSignal(60)
  const [captureNewTags, setCaptureNewTags] = createSignal(true)
  const [notifyOnRun, setNotifyOnRun] = createSignal(true)
  const [configDir, setConfigDir] = createSignal('~/Library/Application Support/taskstream')

  const chooseTheme = (choice: ThemeChoice) => {
    setTheme(choice)
    applyTheme(choice)
  }

  return (
    <ViewShell title="Settings" subtitle="General, sync, and per-app flags live here">
      <div class={group}>
        <div class={groupHead}>
          <span class={groupTitle}>General</span>
          <span class={fieldHint}>Appearance and list behaviour</span>
        </div>
        <article class={card}>
          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>Theme</span>
              <span class={fieldHint}>Controls the app chrome tone.</span>
            </span>
            <div class={row}>
              <For each={THEMES}>
                {(option) => (
                  <button
                    type="button"
                    class={chip}
                    data-active={theme() === option.id ? '' : undefined}
                    aria-pressed={theme() === option.id}
                    onClick={() => chooseTheme(option.id)}
                  >
                    {option.id === 'dark' ? <Moon size={11} /> : null}
                    {option.id === 'light' ? <Sun size={11} /> : null}
                    {option.label}
                  </button>
                )}
              </For>
            </div>
          </div>

          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>Show completed</span>
              <span class={fieldHint}>Keep done items visible in lists.</span>
            </span>
            <button
              type="button"
              class={switchTrack}
              data-on={showCompleted() ? '' : undefined}
              role="switch"
              aria-checked={showCompleted()}
              aria-label="Show completed"
              onClick={() => setShowCompleted((value) => !value)}
            >
              <span class={switchKnob} data-on={showCompleted() ? '' : undefined} />
            </button>
          </div>

          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>Confirm before delete</span>
              <span class={fieldHint}>Ask before destructive removes.</span>
            </span>
            <button
              type="button"
              class={switchTrack}
              data-on={confirmDelete() ? '' : undefined}
              role="switch"
              aria-checked={confirmDelete()}
              aria-label="Confirm before delete"
              onClick={() => setConfirmDelete((value) => !value)}
            >
              <span class={switchKnob} data-on={confirmDelete() ? '' : undefined} />
            </button>
          </div>
        </article>

        <div class={groupHead}>
          <span class={groupTitle}>Sync</span>
          <span class={fieldHint}>When integrations run their passes</span>
        </div>
        <article class={card}>
          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>Automatic sync</span>
              <span class={fieldHint}>Sync integrations on an interval.</span>
            </span>
            <button
              type="button"
              class={switchTrack}
              data-on={autoSync() ? '' : undefined}
              role="switch"
              aria-checked={autoSync()}
              aria-label="Automatic sync"
              onClick={() => setAutoSync((value) => !value)}
            >
              <span class={switchKnob} data-on={autoSync() ? '' : undefined} />
            </button>
          </div>

          <Show when={autoSync()}>
            <div class={settingRow}>
              <span class={settingBody}>
                <span class={settingLabel}>Interval</span>
                <span class={fieldHint}>How often auto sync runs.</span>
              </span>
              <div class={row}>
                <For each={INTERVALS}>
                  {(minutes) => (
                    <button
                      type="button"
                      class={chip}
                      data-active={interval() === minutes ? '' : undefined}
                      onClick={() => setInterval(minutes)}
                    >
                      {minutes < 60 ? `${minutes} min` : minutes < 1440 ? `${minutes / 60} h` : 'Daily'}
                    </button>
                  )}
                </For>
              </div>
            </div>
          </Show>
        </article>

        <div class={groupHead}>
          <span class={groupTitle}>Tags &amp; runs</span>
          <span class={fieldHint}>Defaults for apps and automations</span>
        </div>
        <article class={card}>
          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>New tags capture tasks</span>
              <span class={fieldHint}>Default when attaching an app to a tag.</span>
            </span>
            <button
              type="button"
              class={switchTrack}
              data-on={captureNewTags() ? '' : undefined}
              role="switch"
              aria-checked={captureNewTags()}
              aria-label="New tags capture tasks"
              onClick={() => setCaptureNewTags((value) => !value)}
            >
              <span class={switchKnob} data-on={captureNewTags() ? '' : undefined} />
            </button>
          </div>

          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>Notify on run finished</span>
              <span class={fieldHint}>Surface a status note when this app finishes work.</span>
            </span>
            <button
              type="button"
              class={switchTrack}
              data-on={notifyOnRun() ? '' : undefined}
              role="switch"
              aria-checked={notifyOnRun()}
              aria-label="Notify on run finished"
              onClick={() => setNotifyOnRun((value) => !value)}
            >
              <span class={switchKnob} data-on={notifyOnRun() ? '' : undefined} />
            </button>
          </div>

          <div class={settingRow}>
            <span class={settingBody}>
              <span class={settingLabel}>Defaults</span>
              <span class={fieldHint}>Demo content needs no tag or run settings.</span>
            </span>
            <span class={chip} data-active="">
              <Check size={10} />
              Saved
            </span>
          </div>
        </article>

        <div class={groupHead}>
          <span class={groupTitle}>About</span>
          <span class={fieldHint}>Build and storage details</span>
        </div>
        <article class={card}>
          <div class={field}>
            <span class={fieldLabel}>
              <Clock size={11} /> Version
            </span>
            <span class={fieldHint}>0.14.2 — the build's version, from Cargo.toml.</span>
          </div>

          <div class={field}>
            <span class={fieldLabel}>
              <Folder size={11} /> Config location
            </span>
            <div class={row}>
              <input
                class={textInput}
                value={configDir()}
                aria-label="Config location"
                onInput={(event) => setConfigDir(event.currentTarget.value)}
              />
              <button type="button" class={action} data-muted="">
                Open
              </button>
            </div>
            <span class={fieldHint}>
              Override with <code>MY_TODO_CONFIG_DIR</code>.
            </span>
          </div>

          <div class={divider} />

          <div class={rowBetween}>
            <span class={fieldHint}>
              <User size={11} /> Signed in locally · no account required
            </span>
            <button type="button" class={action} data-muted="">
              Reset demo data
            </button>
          </div>
        </article>
      </div>
    </ViewShell>
  )
}
