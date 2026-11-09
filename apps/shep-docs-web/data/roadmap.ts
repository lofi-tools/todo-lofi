/**
 * The one maintained Shepherd roadmap and availability source. The public
 * roadmap page, the landing availability columns and the docs navigation all
 * read from this file; do not hand-maintain a second list in a page or test.
 *
 * This lives outside `src/` because Panda scans `src/` for style-shaped
 * objects, and these records use keys such as `title`, `status` and `tone`.
 */

export const ROADMAP_PATH = '/docs/users/roadmap'

export type RoadmapStatus = 'now' | 'next' | 'later' | 'exploring'
export type RoadmapTone = 'success' | 'accent' | 'warning'

export interface RoadmapLink {
  label: string
  href: string
}

export interface RoadmapItem {
  id: string
  title: string
  status: RoadmapStatus
  summary: string
  links: RoadmapLink[]
  /** The text used on the landing page; absent means roadmap-only. */
  landing?: string
  /** Phrases that assert this item's public availability; covered across every route. */
  publicClaimTerms?: string[]
}

export interface RoadmapStatusDefinition {
  id: RoadmapStatus
  label: string
  badge: string
  tone: RoadmapTone
  summary: string
  detail: string
  shipped: boolean
  /** Copy variants that mark a status as shipped or not shipped. */
  claimMarkers: string[]
  landingHeading?: string
  landingBadge?: string
  landingSummary?: string
}

export interface AvailabilityGroup {
  id: string
  heading: string
  badge: string
  tone: RoadmapTone
  summary: string
  items: string[]
}

export const roadmapStatuses: RoadmapStatusDefinition[] = [
  {
    id: 'now',
    label: 'Now',
    badge: 'shipped',
    tone: 'success',
    summary: 'Implemented and exercised by the app and its test suites.',
    detail: 'These capabilities are in the current build. They are the only items on this page that are available today.',
    shipped: true,
    claimMarkers: ['available now', 'in this build', 'works today'],
    landingHeading: 'Available now',
    landingBadge: 'in this build',
    landingSummary: 'Implemented and exercised by the app and its test suites.',
  },
  {
    id: 'next',
    label: 'Next',
    badge: 'not shipped',
    tone: 'accent',
    summary: 'Specified and being built, but not in the current build.',
    detail: 'The next slice is under active work. It is not available in the current build, and the app remains the source of truth.',
    shipped: false,
    claimMarkers: [
      'not in the current build',
      'being built',
      'specified',
      'next step',
      'next-step',
      'still being prepared',
      'release roadmap',
      'not available',
      'not current',
      'not yet',
      'no signed',
    ],
    landingHeading: 'Next',
    landingBadge: 'being built',
    landingSummary: 'Specified and actively being built, but not in the current build.',
  },
  {
    id: 'later',
    label: 'Later',
    badge: 'planned',
    tone: 'warning',
    summary: 'Planned, with no promise that it lands in the next release.',
    detail: 'Later work is on the roadmap but not scheduled. It has no date and must not be described as available.',
    shipped: false,
    claimMarkers: ['no date', 'not supported', 'no supported build', 'not part of the current gate'],
    landingHeading: 'Planned',
    landingBadge: 'no date',
    landingSummary: 'On the roadmap, with no promise that it lands in the next release.',
  },
  {
    id: 'exploring',
    label: 'Exploring',
    badge: 'research',
    tone: 'warning',
    summary: 'Research only; not committed to a release.',
    detail: 'Exploring items are possible directions, not planned work. No delivery date or support promise is implied.',
    shipped: false,
    claimMarkers: ['not committed', 'no committed design', 'research only'],
  },
]

export const roadmapItems: RoadmapItem[] = [
  {
    id: 'local-workspace',
    title: 'Several agents and projects in one window',
    status: 'now',
    summary: 'Run parallel tasks across projects, with one task list and one shared inspector.',
    links: [
      { label: 'Mental model', href: '/docs/users/mental-model' },
      { label: 'Install and run', href: '/docs/users/install' },
    ],
    landing: 'Several agents and projects in one window',
  },
  {
    id: 'worktrees',
    title: 'A worktree and branch per task',
    status: 'now',
    summary: 'Agents edit isolated checkouts; review, commit and squash-merge stay attached to the task.',
    links: [{ label: 'Mental model', href: '/docs/users/mental-model' }],
    landing: 'Git worktrees, review, commit and squash-merge',
  },
  {
    id: 'terminals',
    title: 'Shared and task-specific terminals',
    status: 'now',
    summary: 'Project-shared and task-scoped PTYs survive task switches and split inside the Terminals tab.',
    links: [{ label: 'Mental model', href: '/docs/users/mental-model' }],
    landing: 'Project-shared and task-specific terminals',
  },
  {
    id: 'preview',
    title: 'Preview with element annotation',
    status: 'now',
    summary: 'Select an element, write the change and send the annotation; blocked pages fall back to screenshots.',
    links: [
      { label: 'Preview and annotation', href: '/docs/users/preview' },
      { label: 'Annotation API', href: '/docs/contributors/annotation-api' },
    ],
    landing: 'Preview with element annotation and screenshot fallback',
  },
  {
    id: 'prompt-queue',
    title: 'Prompt queues that survive a reload',
    status: 'now',
    summary: 'Queue a follow-up behind a live turn and keep the task workspace recoverable.',
    links: [{ label: 'Mental model', href: '/docs/users/mental-model' }],
    landing: 'Prompt queues that survive a reload',
  },
  {
    id: 'agents',
    title: 'Codex, OpenCode, Claude Code and custom ACP agents',
    status: 'now',
    summary: 'Shepherd starts installed agents, probes their protocol and shows the tier and fallback reason.',
    links: [
      { label: 'Install an agent', href: '/docs/users/install#agents' },
      { label: 'Adapter contracts', href: '/docs/contributors/adapter-contracts' },
    ],
    landing: 'Codex, OpenCode, Claude Code and custom ACP agents',
  },
  {
    id: 'local-data',
    title: 'Local projects, transcripts and settings',
    status: 'now',
    summary: 'Projects, tasks, worktrees, transcripts and settings stay on the machine; no account or telemetry.',
    links: [
      { label: 'Overview', href: '/docs/users' },
      { label: 'Configuration', href: '/docs/users/configuration' },
    ],
  },
  {
    id: 'review-search',
    title: 'Review, task search, notifications and themes',
    status: 'now',
    summary: 'The review surface, cross-task search, opt-in notifications and appearance controls are in the workspace.',
    links: [
      { label: 'Mental model', href: '/docs/users/mental-model' },
      { label: 'Configuration', href: '/docs/users/configuration' },
    ],
  },
  {
    id: 'files-diffs',
    title: 'Read-only files, Git and diffs',
    status: 'now',
    summary: 'Browse the project or worktree, inspect changed files and review diffs without leaving the window.',
    links: [{ label: 'Mental model', href: '/docs/users/mental-model' }],
  },
  {
    id: 'agent-pool',
    title: 'Quota-aware agent pool',
    status: 'next',
    summary: 'Choose the next healthy installed agent when the preferred provider or model is exhausted.',
    links: [
      { label: 'Agent pool', href: '/docs/users/agent-pool' },
      { label: 'Quota ledger', href: '/docs/contributors/quota-ledger' },
    ],
    landing: 'Quota-aware agent pool with priority fallback',
    publicClaimTerms: ['agent pool', 'quota-aware pool'],
  },
  {
    id: 'signed-releases',
    title: 'Signed macOS and Linux release artifacts',
    status: 'next',
    summary: 'Produce signed, verified bundles from the clean-machine release gate.',
    links: [
      { label: 'Releases', href: '/docs/users/releases' },
      { label: 'Release pipeline', href: '/docs/contributors/release-pipeline' },
    ],
    landing: 'Signed macOS and Linux release artifacts',
    publicClaimTerms: ['signed'],
  },
  {
    id: 'release-downloads',
    title: 'A release download path',
    status: 'next',
    summary: 'Resolve the newest signed artifact for the platform without a hand-maintained download list.',
    links: [{ label: 'Releases', href: '/docs/users/releases' }],
    landing: 'Downloads that resolve the newest release',
  },
  {
    id: 'windows',
    title: 'Windows desktop support',
    status: 'later',
    summary: 'Bring the desktop workspace to Windows; no date is promised.',
    links: [{ label: 'Releases', href: '/docs/users/releases' }],
    landing: 'Windows desktop support',
    publicClaimTerms: ['windows'],
  },
  {
    id: 'windows-installer',
    title: 'A signed Windows installer',
    status: 'later',
    summary: 'Package and sign the Windows build after the desktop support lands.',
    links: [{ label: 'Releases', href: '/docs/users/releases' }],
    landing: 'A signed Windows installer',
  },
  {
    id: 'remote-projects',
    title: 'Remote projects over SSH',
    status: 'later',
    summary: 'Open a project on another machine without changing the local-first task model.',
    links: [],
  },
  {
    id: 'agent-teams',
    title: 'Agent teams and orchestration',
    status: 'exploring',
    summary: 'Research into multi-agent coordination; no committed design or date.',
    links: [],
  },
  {
    id: 'free-tier-catalog',
    title: 'Free-tier agent and model catalog',
    status: 'exploring',
    summary: 'A separate catalog for hosted free tiers and local models; it will not become the pool authority.',
    links: [],
  },
]

/** Landing-page availability columns, derived from the same roadmap items. */
export const availability: AvailabilityGroup[] = roadmapStatuses
  .filter((status) => status.shipped || status.landingHeading)
  .map((status) => ({
    id: status.id,
    heading: status.landingHeading ?? status.label,
    badge: status.landingBadge ?? status.badge,
    tone: status.tone,
    summary: status.landingSummary ?? status.summary,
    items: roadmapItems
      .filter((item) => item.status === status.id && item.landing)
      .flatMap((item) => (item.landing ? [item.landing] : [])),
  }))
