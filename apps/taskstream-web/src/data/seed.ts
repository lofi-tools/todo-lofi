/** The shapes the web app mirrors from the desktop store. */

export type Priority = 0 | 1 | 2 | 3

export type DueTone = 'overdue' | 'today' | 'soon' | 'later'

export interface Subtask {
  id: string
  title: string
  done: boolean
}

export interface Task {
  id: string
  title: string
  notes: string
  tagIds: string[]
  completed: boolean
  /** Human label the row and details pane show, or null when undated. */
  due: string | null
  dueTone: DueTone | null
  priority: Priority
  repeat: string | null
  subtasks: Subtask[]
  /** Ids of tasks that must finish first; the list hides the task until they do. */
  blockedBy: string[]
  createdAt: number
}

export interface Tag {
  id: string
  name: string
  /** Accent used for the tag dot, in the same hues as the app. */
  color: string
}

export interface Project {
  id: string
  name: string
  path: string
}

export interface Notification {
  id: string
  title: string
  body: string
  time: string
  unread: boolean
  tone: 'info' | 'success' | 'warning' | 'danger'
}

export const tags: Tag[] = [
  { id: 'inbox', name: 'inbox', color: '#8a8f98' },
  { id: 'work', name: 'work', color: '#5e6ad2' },
  { id: 'life', name: 'life admin', color: '#4cb782' },
  { id: 'home', name: 'home', color: '#c356d4' },
  { id: 'release', name: 'release', color: '#f2994a' },
]

export const projects: Project[] = [
  { id: 'todo-list', name: 'todo-list', path: '~/src/todo-list' },
  { id: 'recipe-app', name: 'recipe app', path: '~/src/recipe-app' },
  { id: 'website', name: 'website', path: '~/src/website' },
]

export const tasks: Task[] = [
  {
    id: 'ship',
    title: 'Ship the release',
    notes: 'Cut the tag, publish the notes, then announce it in the channels.',
    tagIds: ['work', 'release'],
    completed: false,
    due: 'in 5d',
    dueTone: 'later',
    priority: 3,
    repeat: null,
    subtasks: [
      { id: 'ship-1', title: 'Write the changelog', done: true },
      { id: 'ship-2', title: 'Cut the tag', done: false },
      { id: 'ship-3', title: 'Announce it', done: false },
    ],
    blockedBy: [],
    createdAt: 1,
  },
  {
    id: 'sam',
    title: 'Reply to Sam',
    notes: 'The review notes are in the doc; answer the two open questions.',
    tagIds: ['work'],
    completed: false,
    due: 'in 3d',
    dueTone: 'soon',
    priority: 2,
    repeat: null,
    subtasks: [],
    blockedBy: [],
    createdAt: 2,
  },
  {
    id: 'dog',
    title: 'Feed the dog',
    notes: '',
    tagIds: ['home'],
    completed: false,
    due: '6:00 PM',
    dueTone: 'today',
    priority: 1,
    repeat: 'every day',
    subtasks: [],
    blockedBy: [],
    createdAt: 3,
  },
  {
    id: 'plants',
    title: 'Water the plants',
    notes: '',
    tagIds: ['home'],
    completed: false,
    due: '6:00 PM',
    dueTone: 'today',
    priority: 1,
    repeat: 'every 3 days',
    subtasks: [],
    blockedBy: [],
    createdAt: 4,
  },
  {
    id: 'passport',
    title: 'Renew the passport',
    notes: 'Photos are in the drawer; the form needs the old number.',
    tagIds: ['life'],
    completed: false,
    due: 'yesterday',
    dueTone: 'overdue',
    priority: 3,
    repeat: null,
    subtasks: [],
    blockedBy: [],
    createdAt: 5,
  },
  {
    id: 'venue',
    title: 'Book the venue',
    notes: 'Two options left; the smaller one has the better light.',
    tagIds: ['life'],
    completed: false,
    due: 'in 2d',
    dueTone: 'soon',
    priority: 2,
    repeat: null,
    subtasks: [],
    blockedBy: [],
    createdAt: 6,
  },
  {
    id: 'pack',
    title: 'Pack for the trip',
    notes: '',
    tagIds: ['life'],
    completed: false,
    due: 'in 6d',
    dueTone: 'later',
    priority: 1,
    repeat: null,
    subtasks: [
      { id: 'pack-1', title: 'Pack / clothes', done: false },
      { id: 'pack-2', title: 'Pack / food', done: false },
      { id: 'pack-3', title: 'Pack / stay', done: false },
    ],
    // Hidden until the venue is booked, the way the desktop hides blocked work.
    blockedBy: ['venue'],
    createdAt: 7,
  },
  {
    id: 'review',
    title: 'Review PR #482',
    notes: '',
    tagIds: ['work'],
    completed: false,
    due: null,
    dueTone: null,
    priority: 2,
    repeat: null,
    subtasks: [],
    blockedBy: [],
    createdAt: 8,
  },
  {
    id: 'migrate',
    title: 'Migrate database schema',
    notes: '',
    tagIds: ['work'],
    completed: false,
    due: null,
    dueTone: null,
    priority: 1,
    repeat: null,
    subtasks: [{ id: 'migrate-1', title: 'Write migration tests', done: false }],
    blockedBy: ['review'],
    createdAt: 9,
  },
  {
    id: 'invoices',
    title: 'Send the invoices',
    notes: '',
    tagIds: ['life'],
    completed: false,
    due: 'in 9d',
    dueTone: 'later',
    priority: 1,
    repeat: 'every month',
    subtasks: [],
    blockedBy: [],
    createdAt: 10,
  },
  {
    id: 'docs',
    title: 'Read the docs on repeats',
    notes: '',
    tagIds: ['work'],
    completed: true,
    due: null,
    dueTone: null,
    priority: 0,
    repeat: null,
    subtasks: [],
    blockedBy: [],
    createdAt: 11,
  },
  {
    id: 'todoist',
    title: 'Set up Todoist sync',
    notes: '',
    tagIds: ['work'],
    completed: true,
    due: null,
    dueTone: null,
    priority: 0,
    repeat: null,
    subtasks: [],
    blockedBy: [],
    createdAt: 12,
  },
]

export const notifications: Notification[] = [
  {
    id: 'n1',
    title: 'Repeat created “Water the plants”',
    body: 'The every-3-days rule wrote today’s occurrence.',
    time: '2m ago',
    unread: true,
    tone: 'info',
  },
  {
    id: 'n2',
    title: 'Todoist sync finished',
    body: '4 tasks updated in both apps, nothing conflicted.',
    time: '18m ago',
    unread: true,
    tone: 'success',
  },
  {
    id: 'n3',
    title: 'A replay still owes 2 changes',
    body: 'Two local edits could not reach Todoist; they will retry.',
    time: '1h ago',
    unread: false,
    tone: 'warning',
  },
]
