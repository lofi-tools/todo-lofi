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

/**
 * A phase row in the automations panel illustration.
 *
 * Key names avoid CSS property names on purpose (see the note at the top of
 * this file): `content`, `display`, and friends would be read as declarations.
 */
export interface RunPhase {
  label: string
  state: 'done' | 'active' | 'pending'
}

export interface RunAction {
  label: string
  kind: 'primary' | 'quiet' | 'danger'
}

export interface AutomationRun {
  task: string
  round: string
  branch: string
  baseBranch: string
  phases: RunPhase[]
  actions: RunAction[]
  notes: string[]
}

const CODING_ORDER = ['Interview', 'Spec', 'Implement', 'Review', 'Merge']

/** Earlier phases are done, this one is active, the rest are pending. */
const codingPhases = (active: string): RunPhase[] =>
  CODING_ORDER.map((label) => ({
    label,
    state:
      label === active ? 'active' : CODING_ORDER.indexOf(label) < CODING_ORDER.indexOf(active) ? 'done' : 'pending',
  }))

/** Two coding runs, at different phases, as the automations panel groups them. */
export const automationRuns: AutomationRun[] = [
  {
    task: 'Add OAuth login',
    round: 'Round 2 · Implement',
    branch: 'feature/42-add-oauth',
    baseBranch: 'main',
    phases: codingPhases('Implement'),
    actions: [{ label: 'Complete step', kind: 'primary' }],
    notes: [
      'Round 1 rejected · error paths are not covered',
      'Spec saved · docs/spec/add-oauth-spec.md',
    ],
  },
  {
    task: 'Map Todoist sections onto tags',
    round: 'Round 1 · Interview',
    branch: '',
    baseBranch: 'main',
    phases: codingPhases('Interview'),
    actions: [
      { label: 'Approve', kind: 'primary' },
      { label: 'Reject', kind: 'quiet' },
    ],
    notes: ['Interview round 1 · asking clarifying questions'],
  },
]

/** Branches whose runs are gone but which still want deleting. */
export const branchCleanup: { branch: string; note: string }[] = [
  { branch: 'feature/old-notes-pane', note: 'run cancelled' },
]

/** The recipe group above the runs. */
export const automationRecipe = {
  name: 'Coding workflow',
  summary: 'Five phases, each a step you complete. The agent works inside a phase; you decide when it ends.',
  status: 'Enabled',
}

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
