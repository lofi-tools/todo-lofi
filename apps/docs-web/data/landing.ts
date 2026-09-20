/**
 * Illustrative data for the illustrations on the landing page — not real user
 * data. It lives outside `src/` on purpose: Panda CSS scans every file under
 * `src/` for style-shaped objects, and these records use keys that are also CSS
 * property names (`title`, `status`, `id`, `priority`, `meta`), which produced
 * declarations like `meta: round 1;` and broke the CSS build.
 */
import type {
  ChatMessage,
  ColumnStatus,
  Issue,
  KanbanColumn,
  Priority,
  TimelineRow,
} from 'web-design-system/components'

function issue(
  taskId: string,
  taskTitle: string,
  labelList: string[],
  priorityLevel: Priority,
  footnote?: string,
): Issue {
  const row = {} as Issue
  row.id = taskId
  row.title = taskTitle
  row.labels = labelList
  row.priority = priorityLevel
  if (footnote) row.meta = footnote
  return row
}

function column(columnTitle: string, columnStatus: ColumnStatus, issues: Issue[]): KanbanColumn {
  const board = {} as KanbanColumn
  board.title = columnTitle
  board.status = columnStatus
  board.issues = issues
  return board
}

function message(
  role: 'user' | 'agent',
  author: string,
  text: string,
  time?: string,
): ChatMessage {
  const entry = {} as ChatMessage
  entry.role = role
  entry.author = author
  entry.text = text
  if (time) entry.meta = time
  return entry
}

/** The task list as the app renders it: priority, id, title, labels, meta. */
export const taskRows: Issue[] = [
  issue('T-1284', 'Draft the travel checklist templates', ['mini-apps'], 'high', 'Today'),
  issue('T-1291', 'Sync the Helsinki trip project', ['sync', 'travel'], 'medium', 'Tomorrow'),
  issue('T-1296', 'Normalise deadline pressure in the score', ['storage'], 'urgent', 'Overdue'),
  issue('T-1303', 'Review the managed ownership spec', ['spec'], 'low', 'Friday'),
]

/** A week of repeats, with each occurrence's own window. */
export const repeatRows: TimelineRow[] = [
  { title: 'Morning review', start: 1, span: 5, status: 'every day' },
  { title: 'Gym', start: 1, span: 3, status: 'Mon, Wed, Fri' },
  { title: 'Water the plants', start: 2, span: 1, status: 'every 3 days' },
  { title: 'Weekly review', start: 5, span: 1, status: 'Friday' },
]

/** A short agent exchange about work the app really does. */
export const agentThread: ChatMessage[] = [
  message(
    'user',
    'You',
    'Set up a repeat for watering the plants, every three days, starting tomorrow morning.',
    '09:12',
  ),
  message(
    'agent',
    'todo-lofi agent',
    'Added a repeat — every 3 days at 08:00. Tomorrow’s occurrence stays blocked until 08:00, so it will not show up in your list before it is actually doable.',
    '09:12',
  ),
  message('user', 'You', 'Good. Take T-1303 through the coding workflow.', '09:14'),
  message(
    'agent',
    'todo-lofi agent',
    'Started a coding run on T-1303, now in Interview. I will ask questions before anything is written, then turn the answers into a spec.',
    '09:14',
  ),
]

/** Coding work as the workflow stages hold it. */
export const codingColumns: KanbanColumn[] = [
  column('Interview', 'todo', [
    issue('T-1303', 'Managed ownership spec', ['spec'], 'high', 'round 1'),
    issue('T-1309', 'Todoist section mapping', ['sync'], 'medium'),
  ]),
  column('Spec', 'in-progress', [
    issue('T-1296', 'Priority scoring notes', ['storage'], 'medium', 'approved'),
  ]),
  column('Implement', 'in-progress', [
    issue('T-1284', 'Trip checklist templates', ['mini-apps'], 'high', 'branch trip-templates'),
  ]),
  column('Review', 'done', [
    issue('T-1272', 'Repeat picker keyboard flow', ['tasklist'], 'low', 'merged'),
  ]),
]
