import type { ChatMessage } from 'web-design-system/components'

export const REPO_URL = 'https://github.com/lofi-tools/todo-lofi'

/**
 * Copy and illustrative data for Shepherd's landing page.
 *
 * This lives outside `src/` for the same reason `landing.ts` does: Panda scans
 * every file under `src/` for style-shaped objects, and these records use keys
 * such as `title`, `body`, and `status` that it would read as declarations.
 */

export type ReasonIcon = 'parallel' | 'local' | 'review'

export interface ReasonCard {
  icon: ReasonIcon
  title: string
  body: string
}

export const reasonCards: ReasonCard[] = [
  {
    icon: 'parallel',
    title: 'Run work in parallel',
    body: 'Start a task for each agent and project. One can wait on approval while another keeps working; the sidebar keeps running and review counts visible across the window.',
  },
  {
    icon: 'local',
    title: 'Keep the work local',
    body: 'Projects, tasks, worktrees, transcripts and settings live on your machine. Shepherd uses the agent CLIs you already installed and authenticated.',
  },
  {
    icon: 'review',
    title: 'Review before it lands',
    body: 'Every task has a real branch and diff. Approve the tool call, send a review note, or squash-merge when the change is ready.',
  },
]

export type ReviewIcon = 'worktrees' | 'approvals' | 'preview' | 'terminals'

export interface ReviewCard {
  icon: ReviewIcon
  title: string
  body: string
}

export const reviewCards: ReviewCard[] = [
  {
    icon: 'worktrees',
    title: 'A worktree per task',
    body: 'Agents do not edit the same checkout. Each task gets its own Git worktree, and the Review tab shows the changed files, the diff and the branch before anything is committed.',
  },
  {
    icon: 'approvals',
    title: 'Approvals in context',
    body: 'Permission requests name the tool, its arguments and the paths it touches. When a patch is available, the affected diff sits inside the approval card.',
  },
  {
    icon: 'preview',
    title: 'Preview and annotation',
    body: 'Open the project in the inspector, select an element and send the note to the agent as a change request. Pages that block injection fall back to screenshot coordinates.',
  },
  {
    icon: 'terminals',
    title: 'Terminals that survive a tab',
    body: 'Project-shared and task-specific terminals keep running while you switch tasks. Quitting ends them, and the next launch says so instead of pretending they survived.',
  },
]

export type AvailabilityTone = 'success' | 'accent' | 'warning'

export interface AvailabilityGroup {
  id: string
  heading: string
  badge: string
  tone: AvailabilityTone
  summary: string
  items: string[]
}

export const availability: AvailabilityGroup[] = [
  {
    id: 'now',
    heading: 'Available now',
    badge: 'in this build',
    tone: 'success',
    summary: 'Implemented and exercised by the app and its test suites.',
    items: [
      'Several agents and projects in one window',
      'Git worktrees, review, commit and squash-merge',
      'Project-shared and task-specific terminals',
      'Preview with element annotation and screenshot fallback',
      'Prompt queues that survive a reload',
      'Codex, OpenCode, Claude Code and custom ACP agents',
    ],
  },
  {
    id: 'next',
    heading: 'Next',
    badge: 'being built',
    tone: 'accent',
    summary: 'Specified and actively being built, but not in the current build.',
    items: [
      'Quota-aware agent pool with priority fallback',
      'Signed macOS and Linux release artifacts',
      'Downloads that resolve the newest release',
    ],
  },
  {
    id: 'planned',
    heading: 'Planned',
    badge: 'no date',
    tone: 'warning',
    summary: 'On the roadmap, with no promise that it lands in the next release.',
    items: ['Windows desktop support', 'A signed Windows installer'],
  },
]

/** An illustrative task turn; the workspace mock is not a real session. */
export const workspaceThread: ChatMessage[] = [
  {
    role: 'user',
    author: 'You',
    text: 'Add OAuth login to the API, with tests for the callback and the error path.',
    meta: '09:41',
  },
  {
    role: 'agent',
    author: 'Codex · working',
    text: 'I’ll interview first, then write the spec and implement it in feature/42-add-oauth. The callback test is the first thing I’ll pin down.',
    meta: '09:42',
  },
]
