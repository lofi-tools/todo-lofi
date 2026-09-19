/**
 * Shared props for the mock product-UI components. Keeping them in a `.ts`
 * module (not in an `.astro` frontmatter) keeps the types importable across
 * components.
 */

export type Priority = 'urgent' | 'high' | 'medium' | 'low' | 'none'

export interface Issue {
  id: string
  title: string
  labels?: string[]
  assignees?: string[]
  priority?: Priority
  meta?: string
}

export type ColumnStatus = 'backlog' | 'todo' | 'in-progress' | 'done'

export interface KanbanColumn {
  title: string
  status: ColumnStatus
  issues: Issue[]
}

export interface TimelineRow {
  title: string
  start: number
  span: number
  status?: string
}

export interface ActivityItem {
  actor: string
  action: string
  target?: string
  time: string
}

export interface ChatMessage {
  role: 'user' | 'agent'
  author: string
  text: string
  meta?: string
}
