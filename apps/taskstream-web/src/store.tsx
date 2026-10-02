import { createContext, createMemo, useContext, type Accessor, type JSX } from 'solid-js'
import { createStore, produce } from 'solid-js/store'
import {
  notifications as seedNotifications,
  projects as seedProjects,
  tags as seedTags,
  tasks as seedTasks,
  type DueTone,
  type Notification,
  type Priority,
  type Project,
  type Subtask,
  type Tag,
  type Task,
} from './data/seed'

/** Where the nav bar can point, mirroring the desktop's destinations. */
export type Destination =
  | { kind: 'all' }
  | { kind: 'tag'; tagId: string }
  | { kind: 'apps' }
  | { kind: 'integrations' }
  | { kind: 'automations' }
  | { kind: 'sync' }
  | { kind: 'settings' }

/** Which pane the right column shows, the desktop's `RightPane`. */
export type RightPane = 'details' | 'agent' | 'split'

export type SectionTone = 'overdue' | 'today' | 'soon' | 'later' | 'none'

export interface Section {
  id: SectionTone
  title: string
  tasks: Task[]
}

interface AppState {
  tasks: Task[]
  tags: Tag[]
  projects: Project[]
  notifications: Notification[]
  destination: Destination
  rightPane: RightPane
  /** Task ids visited, newest last; drives the title bar's back/forward. */
  history: string[]
  historyIndex: number
  notificationsOpen: boolean
  syncHistoryOpen: boolean
  commandOpen: boolean
  draft: string
}

const SECTION_ORDER: { id: SectionTone; title: string }[] = [
  { id: 'overdue', title: 'Overdue' },
  { id: 'today', title: 'Today' },
  { id: 'soon', title: 'Upcoming' },
  { id: 'later', title: 'Later' },
  { id: 'none', title: 'No date' },
]

const bucket = (tone: DueTone | null): SectionTone => tone ?? 'none'

let counter = 100
const nextId = (prefix: string) => `${prefix}-${counter++}`

function createAppState() {
  const [state, setState] = createStore<AppState>({
    tasks: seedTasks,
    tags: seedTags,
    projects: seedProjects,
    notifications: seedNotifications,
    destination: { kind: 'all' },
    rightPane: 'split',
    history: [],
    historyIndex: -1,
    notificationsOpen: false,
    syncHistoryOpen: false,
    commandOpen: false,
    draft: '',
  })

  const selectedTaskId = () => state.history[state.historyIndex] ?? null
  const selectedTask = createMemo(() => {
    const id = selectedTaskId()
    return id ? (state.tasks.find((task) => task.id === id) ?? null) : null
  })

  const isBlocked = (task: Task) =>
    task.blockedBy.some((id) => {
      const blocker = state.tasks.find((other) => other.id === id)
      return blocker ? !blocker.completed : false
    })

  const blockedByTitles = (task: Task) =>
    task.blockedBy
      .map((id) => state.tasks.find((other) => other.id === id)?.title)
      .filter((title): title is string => Boolean(title))

  /** The tasks the current destination shows, blocked ones already removed. */
  const visibleTasks = createMemo(() => {
    const open = state.tasks.filter((task) => !task.completed && !isBlocked(task))
    const destination = state.destination
    if (destination.kind === 'tag') {
      return open.filter((task) => task.tagIds.includes(destination.tagId))
    }
    return open
  })

  const completedTasks = createMemo(() => {
    const done = state.tasks.filter((task) => task.completed)
    const destination = state.destination
    if (destination.kind === 'tag') {
      return done.filter((task) => task.tagIds.includes(destination.tagId))
    }
    return done
  })

  /** Open tasks grouped into the due buckets the list draws. */
  const sections = createMemo<Section[]>(() => {
    const visible = visibleTasks()
    return SECTION_ORDER.map((section) => ({
      ...section,
      tasks: visible.filter((task) => bucket(task.dueTone) === section.id),
    })).filter((section) => section.tasks.length > 0)
  })

  const tagById = (id: string) => state.tags.find((tag) => tag.id === id)
  const tagsFor = (task: Task) =>
    task.tagIds.map((id) => tagById(id)).filter((tag): tag is Tag => Boolean(tag))

  const destinationLabel = () => {
    const destination = state.destination
    if (destination.kind === 'all') return 'All tasks'
    if (destination.kind === 'tag') return `#${tagById(destination.tagId)?.name ?? ''}`
    const labels: Record<string, string> = {
      apps: 'Apps',
      integrations: 'Integrations',
      automations: 'Automations',
      sync: 'Sync history',
      settings: 'Settings',
    }
    return labels[destination.kind] ?? 'Taskstream'
  }

  const setDestination = (destination: Destination) => {
    setState('destination', destination)
    setState('history', [])
    setState('historyIndex', -1)
  }

  const selectTask = (id: string | null) => {
    if (id === null) {
      setState('history', [])
      setState('historyIndex', -1)
      return
    }
    const current = selectedTaskId()
    if (current === id) return
    const next = state.history.slice(0, state.historyIndex + 1)
    next.push(id)
    setState('history', next)
    setState('historyIndex', next.length - 1)
  }

  const canGoBack = () => state.historyIndex > 0
  const canGoForward = () => state.historyIndex < state.history.length - 1
  const goBack = () => {
    if (canGoBack()) setState('historyIndex', state.historyIndex - 1)
  }
  const goForward = () => {
    if (canGoForward()) setState('historyIndex', state.historyIndex + 1)
  }

  const updateTask = (id: string, patch: Partial<Task>) => {
    setState(
      'tasks',
      (task) => task.id === id,
      produce((task) => Object.assign(task, patch)),
    )
  }

  const toggleComplete = (id: string) => {
    const task = state.tasks.find((entry) => entry.id === id)
    if (!task) return
    updateTask(id, { completed: !task.completed })
  }

  const addTask = (title: string, tagId?: string) => {
    const trimmed = title.trim()
    if (!trimmed) return
    const tagIds = tagId
      ? [tagId]
      : state.destination.kind === 'tag'
        ? [state.destination.tagId]
        : ['inbox']
    const task: Task = {
      id: nextId('task'),
      title: trimmed,
      notes: '',
      tagIds,
      completed: false,
      due: null,
      dueTone: null,
      priority: 0,
      repeat: null,
      subtasks: [],
      blockedBy: [],
      createdAt: Date.now(),
    }
    setState('tasks', (tasks) => [task, ...tasks])
    selectTask(task.id)
  }

  const addSubtask = (taskId: string, title: string) => {
    const trimmed = title.trim()
    if (!trimmed) return
    const subtask: Subtask = { id: nextId('sub'), title: trimmed, done: false }
    setState(
      'tasks',
      (task) => task.id === taskId,
      produce((task) => {
        task.subtasks.push(subtask)
      }),
    )
  }

  const toggleSubtask = (taskId: string, subtaskId: string) => {
    setState(
      'tasks',
      (task) => task.id === taskId,
      produce((task) => {
        const subtask = task.subtasks.find((entry) => entry.id === subtaskId)
        if (subtask) subtask.done = !subtask.done
      }),
    )
  }

  const toggleTag = (taskId: string, tagId: string) => {
    setState(
      'tasks',
      (task) => task.id === taskId,
      produce((task) => {
        const index = task.tagIds.indexOf(tagId)
        if (index >= 0) task.tagIds.splice(index, 1)
        else task.tagIds.push(tagId)
      }),
    )
  }

  const addTag = (name: string) => {
    const trimmed = name.trim().replace(/^#/, '')
    if (!trimmed) return null
    const existing = state.tags.find((tag) => tag.name === trimmed)
    if (existing) return existing.id
    const tag: Tag = {
      id: nextId('tag'),
      name: trimmed,
      color: '#5e6ad2',
    }
    setState('tags', (tags) => [...tags, tag])
    return tag.id
  }

  const setRightPane = (pane: RightPane) => setState('rightPane', pane)

  const openNotifications = () => setState('notificationsOpen', true)
  const closeNotifications = () => setState('notificationsOpen', false)
  const markAllRead = () =>
    setState(
      'notifications',
      produce((list) => {
        for (const notice of list) notice.unread = false
      }),
    )
  const clearNotifications = () => setState('notifications', [])

  const toggleSyncHistory = () => setState('syncHistoryOpen', (open) => !open)
  const toggleCommand = () => setState('commandOpen', (open) => !open)

  const unreadCount = () => state.notifications.filter((notice) => notice.unread).length

  return {
    state,
    // derived
    selectedTaskId,
    selectedTask,
    visibleTasks,
    completedTasks,
    sections,
    isBlocked,
    blockedByTitles,
    tagsFor,
    tagById,
    destinationLabel,
    canGoBack,
    canGoForward,
    unreadCount,
    // actions
    setDestination,
    selectTask,
    goBack,
    goForward,
    updateTask,
    toggleComplete,
    addTask,
    addSubtask,
    toggleSubtask,
    toggleTag,
    addTag,
    setRightPane,
    openNotifications,
    closeNotifications,
    markAllRead,
    clearNotifications,
    toggleSyncHistory,
    toggleCommand,
  }
}

export type AppStore = ReturnType<typeof createAppState>

const AppContext = createContext<AppStore>()

export function AppProvider(props: { children: JSX.Element }) {
  const store = createAppState()
  return <AppContext.Provider value={store}>{props.children}</AppContext.Provider>
}

export function useApp(): AppStore {
  const store = useContext(AppContext)
  if (!store) throw new Error('useApp must be used inside <AppProvider>')
  return store
}

export type { DueTone, Priority, Tag, Task, Subtask, Notification, Project }
