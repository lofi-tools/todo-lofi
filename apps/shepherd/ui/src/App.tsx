/**
 * The fixed task-workspace shell: sidebar, required chat, always-on inspector
 * and status bar. Regions never move between each other. Until the task flow is
 * wired, each region shows its real empty state instead of fake content.
 */

interface InspectorTab {
  id: string
  label: string
  enabled: boolean
}

const INSPECTOR_TABS: InspectorTab[] = [
  { id: 'terminals', label: 'Terminals', enabled: true },
  { id: 'preview', label: 'Preview', enabled: false },
  { id: 'review', label: 'Review', enabled: false },
  { id: 'git', label: 'Git', enabled: false },
  { id: 'files', label: 'Files', enabled: false },
]

export function App() {
  return (
    <div class="shell">
      <div class="shell__body">
        <Sidebar />
        <Chat />
        <Inspector />
      </div>
      <StatusBar />
    </div>
  )
}

function Sidebar() {
  return (
    <aside class="sidebar" aria-label="Projects">
      <h2 class="sidebar__title">Projects</h2>
      <p class="empty">No projects yet.</p>
    </aside>
  )
}

function Chat() {
  return (
    <section class="chat" aria-label="Task chat">
      <header class="chat__header">
        <span class="chat__title">No task selected</span>
      </header>

      <div class="chat__transcript">
        <p class="empty">Select a task to read its transcript.</p>
      </div>

      <div class="composer">
        <textarea
          class="composer__input"
          aria-label="Prompt"
          placeholder="Send a prompt"
          rows={2}
          disabled
        />
        <button type="button" class="composer__send" disabled>
          Send
        </button>
      </div>
    </section>
  )
}

function Inspector() {
  return (
    <aside class="inspector" aria-label="Inspector">
      <div
        class="inspector__tabs"
        role="tablist"
        aria-orientation="vertical"
        aria-label="Inspector tabs"
      >
        {INSPECTOR_TABS.map((tab) => (
          <button
            type="button"
            role="tab"
            class="inspector__tab"
            id={`inspector-tab-${tab.id}`}
            aria-selected={tab.id === 'terminals'}
            aria-disabled={!tab.enabled}
            aria-controls="inspector-pane"
            disabled={!tab.enabled}
            title={tab.enabled ? tab.label : `${tab.label} arrives in a later slice`}
          >
            {tab.label}
          </button>
        ))}
      </div>

      <div
        class="inspector__pane"
        id="inspector-pane"
        role="tabpanel"
        aria-labelledby="inspector-tab-terminals"
      >
        <p class="empty">No terminals yet.</p>
      </div>
    </aside>
  )
}

function StatusBar() {
  return (
    <footer class="status-bar" aria-label="Status">
      <span>no agent</span>
      <span class="status-bar__dot" aria-hidden="true">
        ·
      </span>
      <span>0 tasks running</span>
      <span class="status-bar__dot" aria-hidden="true">
        ·
      </span>
      <span>0 in review</span>
      <span class="status-bar__dot" aria-hidden="true">
        ·
      </span>
      <span>branch —</span>
      <span class="status-bar__dot" aria-hidden="true">
        ·
      </span>
      <span>0 changed</span>
    </footer>
  )
}
