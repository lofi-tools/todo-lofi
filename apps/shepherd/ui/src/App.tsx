/**
 * Placeholder surface for the walking skeleton. The fixed regions (sidebar,
 * chat, inspector, status bar) replace this body once the command bridge is
 * wired; keeping it empty avoids inventing a second state model.
 */
export function App() {
  return (
    <main class="shell">
      <header class="shell__header">
        <span class="shell__brand">Shepherd</span>
        <span class="shell__status">shell scaffold</span>
      </header>
      <p class="shell__note">
        The window is up. Projects, tasks and the transcript arrive with the
        core bridge.
      </p>
    </main>
  )
}
