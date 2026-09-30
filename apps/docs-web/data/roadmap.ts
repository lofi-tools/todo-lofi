/**
 * The `/roadmap` page's buckets and entries — what the app does today, what is
 * being built, what is only sketched, and what is deliberately not planned.
 *
 * This replaces the feature inventory that used to live on `/docs/status`.
 *
 * Lives outside `src/` for the reason `data/landing.ts` does: Panda CSS scans
 * every file under `src/` and reads records whose keys resemble CSS properties
 * as style declarations.
 */

export type RoadmapTone = 'success' | 'accent' | 'warning' | 'info'

export interface RoadmapItem {
  name: string
  note: string
  /** The docs page that covers it, when one exists. */
  href?: string
}

export interface RoadmapBucket {
  id: string
  heading: string
  badge: string
  tone: RoadmapTone
  summary: string
  items: RoadmapItem[]
}

export const roadmapBuckets: RoadmapBucket[] = [
  {
    id: 'now',
    heading: 'Now',
    badge: 'available',
    tone: 'success',
    summary: 'Built, used every day, and not expected to change shape.',
    items: [
      {
        name: 'A local task list',
        note: 'Tasks, nested tags, projects and areas, and drag-and-drop ordering — in a database file you own.',
        href: '/docs/tasks',
      },
      {
        name: 'Priority that sorts itself',
        note: 'Importance and urgency combined with deadline pressure, so the next thing to do is at the top.',
        href: '/docs/tasks#priority',
      },
      {
        name: 'Subtasks, blockers, and dependencies',
        note: 'Blocked-by chains, softer ordering hints, and tags that flow down from a parent task.',
        href: '/docs/tasks#dependencies',
      },
      {
        name: 'Repeat and scheduling',
        note: 'Recurring tasks configured in a picker, with occurrences that stay hidden until they are actually doable.',
        href: '/docs/repeats',
      },
      {
        name: 'The integrated agent pane',
        note: 'Docked beside the list, working in the same tasks you see, with the tools it calls visible.',
        href: '/docs/agent',
      },
      {
        name: 'Configurable model providers',
        note: 'OpenAI-compatible providers, per-model tuning, and fallback combinations.',
        href: '/docs/configuration',
      },
      {
        name: 'Todoist two-way sync',
        note: 'Projects map onto projects, sections onto child tags, and edits travel both ways.',
        href: '/docs/sync#todoist',
      },
      {
        name: 'Travel checklist mini-app',
        note: 'Give it a trip and it generates the packing list and pre-departure chores as regular tasks.',
        href: '/docs/mini-apps',
      },
    ],
  },
  {
    id: 'next',
    heading: 'Next',
    badge: 'preview',
    tone: 'accent',
    summary: 'Usable, but the design is still moving — expect the shape to change.',
    items: [
      {
        name: 'GitHub issues and projects sync',
        note: 'The same sync surface as Todoist, mapped onto GitHub instead.',
        href: '/docs/sync#github',
      },
      {
        name: 'Git-backed coding workflow',
        note: 'Runs that work in a real checkout and produce real diffs.',
        href: '/docs/workflows#coding',
      },
      {
        name: 'Workflow recipes settling',
        note: 'The v2 model for recipes, runs, and the steps that only appear once their prerequisites are met.',
        href: '/docs/workflows',
      },
    ],
  },
  {
    id: 'later',
    heading: 'Later',
    badge: 'coming soon',
    tone: 'warning',
    summary: 'Specified or sketched, not built.',
    items: [
      {
        name: 'Message follow-ups and birthday wishes',
        note: 'Keeping in touch with people, as tasks with a natural cadence.',
        href: '/docs/mini-apps#planned',
      },
      {
        name: 'Job application tracking',
        note: 'A pipeline rather than a list: applied, interviewing, offer, closed.',
        href: '/docs/mini-apps#planned',
      },
      {
        name: 'Outside apps managing part of your lists',
        note: 'Let another program own a section of the tree and keep it in sync.',
        href: '/docs/mini-apps#planned',
      },
    ],
  },
  {
    id: 'exploring',
    heading: 'Exploring',
    badge: 'no promises',
    tone: 'info',
    summary: 'Ideas worth a look, with nothing committed behind them.',
    items: [
      {
        name: 'Due reminders outside the app',
        note: 'The app already notices failures out loud; telling you a task is due while you are elsewhere is a separate, unstarted piece of work.',
      },
      {
        name: 'A mobile companion',
        note: 'Reading and capturing tasks away from the desktop.',
      },
      {
        name: 'Tying a tag to a directory',
        note: 'Point a tag at a folder on disk, for working in plain files alongside your tasks.',
      },
      {
        name: 'More keyboard coverage',
        note: 'The jump-to-task and move-to-project pickers exist; extending that to every action is open.',
      },
    ],
  },
]

/** The short list of things the project has decided against. */
export const notPlanned = [
  'Team and collaboration features. todo-lofi is a personal tool: one person, one list, no accounts to share and no server holding your work.',
]
