import { Match, Switch } from 'solid-js'
import { css } from 'styled-system/css'
import { AppProvider, useApp } from './store'
import TitleBar from './components/TitleBar'
import NavBar from './components/NavBar'
import FooterBar from './components/FooterBar'
import TaskList from './components/TaskList'
import RightRail from './components/RightRail'
import CommandPalette from './components/CommandPalette'
import AppsView from './views/AppsView'
import IntegrationsView from './views/IntegrationsView'
import AutomationsView from './views/AutomationsView'
import SyncHistoryView from './views/SyncHistoryView'
import SettingsView from './views/SettingsView'

/** The app column: title bar, content, footer, with the overlays on top. */
const shell = css({
  display: 'flex',
  flexDirection: 'column',
  h: 'full',
  minH: '0',
  bg: 'canvas',
  color: 'fg.default',
  fontSize: 'sm',
})

const body = css({ display: 'flex', flex: '1', minH: '0', position: 'relative' })

const workspace = css({ display: 'flex', flex: '1', minW: '0', minH: '0', position: 'relative' })

/** The task destinations share one workspace: the list plus the right rail. */
function TaskWorkspace() {
  return (
    <div class={workspace}>
      <TaskList />
      <RightRail />
    </div>
  )
}

/**
 * The window-matching view for the current destination. Task destinations
 * render the workspace; every other destination is a full-width panel.
 */
function Content() {
  const app = useApp()
  return (
    <Switch>
      <Match when={app.state.destination.kind === 'all' || app.state.destination.kind === 'tag'}>
        <TaskWorkspace />
      </Match>
      <Match when={app.state.destination.kind === 'apps'}>
        <AppsView />
      </Match>
      <Match when={app.state.destination.kind === 'integrations'}>
        <IntegrationsView />
      </Match>
      <Match when={app.state.destination.kind === 'automations'}>
        <AutomationsView />
      </Match>
      <Match when={app.state.destination.kind === 'sync'}>
        <SyncHistoryView />
      </Match>
      <Match when={app.state.destination.kind === 'settings'}>
        <SettingsView />
      </Match>
    </Switch>
  )
}

export default function App() {
  return (
    <AppProvider>
      <div class={shell}>
        <TitleBar />
        <div class={body}>
          <NavBar />
          <Content />
        </div>
        <FooterBar />
        <CommandPalette />
      </div>
    </AppProvider>
  )
}
