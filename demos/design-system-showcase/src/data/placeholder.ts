import type {
  ActivityItem,
  ChatMessage,
  Issue,
  KanbanColumn,
  TimelineRow,
} from 'web-design-system/components'

/**
 * Placeholder content only (spec §4.5). Every name, company, issue, quote,
 * and metric here is invented; nothing is Linear's copy.
 */

export const logos = ['Northwind', 'Acme Corp', 'Globex', 'Initech', 'Umbrella']

export const kanban: KanbanColumn[] = [
  {
    title: 'Backlog',
    status: 'backlog',
    issues: [
      { id: 'ENG-2085', title: 'Reduce flicker during sync', labels: ['Performance'], priority: 'medium' },
      { id: 'ENG-2094', title: 'Buffer event stream updates', labels: ['Improvement'], priority: 'low' },
      { id: 'ENG-2200', title: 'Fix delayed route updates', labels: ['Bug'], priority: 'high' },
    ],
  },
  {
    title: 'Todo',
    status: 'todo',
    issues: [
      { id: 'ENG-926', title: 'Remove UI inconsistencies', labels: ['Bug', 'Design'], priority: 'medium' },
      { id: 'ENG-2088', title: 'TypeError when loading profile', labels: ['Bug'], priority: 'urgent' },
      { id: 'ENG-1882', title: 'Optimize cold start', labels: ['Performance'], priority: 'medium' },
    ],
  },
  {
    title: 'In Progress',
    status: 'in-progress',
    issues: [
      {
        id: 'ENG-1487',
        title: 'Remove legacy endpoint from API',
        labels: ['Improvement'],
        priority: 'high',
        assignees: ['Ada Lovelace', 'Grace Hopper'],
      },
      { id: 'MKT-1028', title: 'Launch page assets', labels: ['Design'], priority: 'low' },
    ],
  },
  {
    title: 'Done',
    status: 'done',
    issues: [
      { id: 'ENG-2074', title: 'Clean up deprecated APIs', labels: ['Improvement'], priority: 'low' },
      { id: 'ENG-1960', title: 'Improve fallback messaging', labels: ['Design'], priority: 'medium' },
    ],
  },
]

export const issueList: Issue[] = [
  { id: 'ENG-2061', title: 'Add granular project permissions', labels: ['Feature'], priority: 'high' },
  { id: 'ENG-2483', title: 'Let guests access multiple teams', labels: ['Feature', 'Design'], priority: 'medium' },
  { id: 'ENG-2107', title: 'Create custom roles with scoped access', labels: ['Feature'], priority: 'low' },
]

export const timelineRows: TimelineRow[] = [
  { title: 'UI refresh', start: 1, span: 3, status: 'GA' },
  { title: 'Core screens', start: 3, span: 2, status: 'Beta' },
  { title: 'Split fares', start: 5, span: 3, status: 'Internal' },
  { title: 'Reliability', start: 7, span: 3, status: 'Alpha' },
]

export const timelineMonths = ['MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT']

export const activity: ActivityItem[] = [
  { actor: 'Agent', action: 'created the issue via Slack on behalf of', target: 'Ada', time: '2 min ago' },
  { actor: 'Triage', action: 'added the labels Performance and iOS', time: '2 min ago' },
  { actor: 'Agent', action: 'moved the issue from Todo to', target: 'In Progress', time: 'just now' },
  { actor: 'Agent', action: 'opened a draft pull request', target: '#412', time: 'just now' },
]

export const chat: ChatMessage[] = [
  {
    role: 'user',
    author: 'Ada',
    text: 'What are the three most important customer requests around permissions? Add them to the Access Controls project.',
  },
  {
    role: 'agent',
    author: 'Agent',
    meta: 'Worked for 8 sec',
    text: 'Three requests ranked by customer impact: granular project permissions, guest access across teams, and custom roles with scoped access. I added them to Access Controls.',
  },
  {
    role: 'user',
    author: 'Ada',
    text: 'Review today’s triage and group the issues by what should happen next.',
  },
]

export const changelog = [
  {
    title: 'Recurring workflows for teams',
    date: 'Sep 10, 2026',
    excerpt:
      'Workflows can now respond to more workspace activity and post updates back to the tools your team already uses.',
    featured: true,
  },
  {
    title: 'Priority inbox',
    date: 'Sep 3, 2026',
    excerpt: 'A new tab separates what needs your attention from what can wait until later.',
  },
  {
    title: 'Coding sessions',
    date: 'Aug 19, 2026',
    excerpt: 'Agents can set up, run, and test code before returning work, so fewer handoffs are needed.',
  },
  {
    title: 'Team initiatives',
    date: 'Aug 13, 2026',
    excerpt: 'Assign a team to lead an initiative so it is clear who drives the work forward.',
  },
]

export const testimonials = [
  {
    quote: 'You will probably build a better product, just because of the craft this workflow infuses on your brain.',
    name: 'Ada Lovelace',
    role: 'Staff Engineer, Northwind',
  },
  {
    quote: 'Our speed is intense and this keeps us action biased.',
    name: 'Grace Hopper',
    role: 'Head of Engineering, Acme Corp',
  },
  {
    quote: 'It has the right opinions for fast moving teams.',
    name: 'Alan Turing',
    role: 'CTO, Globex',
  },
]

export const customers = [
  {
    company: 'Northwind',
    quote: 'We cut planning overhead in half within a quarter.',
    name: 'Ada Lovelace',
    role: 'VP Engineering',
    metricValue: '2×',
    metricLabel: 'faster planning cycles',
  },
  {
    company: 'Acme Corp',
    quote: 'The workflow finally matches how our team actually ships.',
    name: 'Grace Hopper',
    role: 'Head of Product',
    metricValue: '−40%',
    metricLabel: 'fewer status meetings',
  },
  {
    company: 'Globex',
    quote: 'Our agents and engineers work out of the same queue now.',
    name: 'Alan Turing',
    role: 'Director of Engineering',
    metricValue: '3×',
    metricLabel: 'throughput per squad',
  },
]

export const pricingPlans = [
  {
    tier: 'Free',
    price: '$0',
    note: 'Free for everyone',
    features: ['Unlimited members', '2 teams', '250 issues', 'Agent platform'],
    cta: 'Get started',
  },
  {
    tier: 'Basic',
    price: '$10',
    cadence: 'per user/month',
    note: 'Billed yearly',
    features: ['All Free features', '5 teams', 'Unlimited issues', 'Unlimited file uploads', 'Admin roles'],
    cta: 'Get started',
  },
  {
    tier: 'Business',
    price: '$16',
    cadence: 'per user/month',
    note: 'Billed yearly',
    features: [
      'All Basic features',
      'Unlimited teams',
      'Private teams and guests',
      'Triage intelligence',
      'Advanced analytics',
    ],
    cta: 'Get started',
    featured: true,
  },
  {
    tier: 'Enterprise',
    price: 'Custom',
    note: 'Annual billing only',
    features: [
      'All Business features',
      'Invoice and PO billing',
      'SAML and SCIM',
      'Granular admin controls',
      'Priority support',
    ],
    cta: 'Contact sales',
  },
]

export const comparisonColumns = ['Free', 'Basic', 'Business', 'Enterprise']

export const comparisonSections = [
  {
    heading: 'Members',
    rows: [
      { label: 'Members', values: ['Unlimited', 'Unlimited', 'Unlimited', 'Unlimited'] },
      { label: 'File upload', values: ['10MB', 'Unlimited', 'Unlimited', 'Unlimited'] },
      { label: 'Teams', values: ['2', '5', 'Unlimited', 'Unlimited'] },
    ],
  },
  {
    heading: 'Core',
    rows: [
      { label: 'Issues, projects, cycles', values: ['✓', '✓', '✓', '✓'] },
      { label: 'API and webhook access', values: ['✓', '✓', '✓', '✓'] },
      { label: 'Issue sync', values: ['—', '✓', '✓', '✓'] },
      { label: 'Guided reviews', values: ['—', '—', '✓', '✓'] },
    ],
  },
  {
    heading: 'AI and agent workflows',
    rows: [
      { label: 'Agent platform', values: ['✓', '✓', '✓', '✓'] },
      { label: 'Coding sessions', values: ['—', '—', '✓', '✓'] },
      { label: 'Triage intelligence', values: ['—', '—', '✓', '✓'] },
    ],
  },
  {
    heading: 'Security',
    rows: [
      { label: 'SSO', values: ['Google', 'Google', 'Google', 'Google + SAML'] },
      { label: 'SCIM provisioning', values: ['—', '—', '—', '✓'] },
      { label: 'Audit log', values: ['—', '—', '—', '✓'] },
    ],
  },
]

export const docsGroups = [
  {
    heading: 'Getting started',
    items: [
      { label: 'Introduction', href: '/replicas/docs', active: true },
      { label: 'Quickstart', href: '/replicas/docs#quickstart' },
      { label: 'Core concepts', href: '/replicas/docs#concepts' },
    ],
  },
  {
    heading: 'Guides',
    items: [
      { label: 'Issues and projects', href: '/replicas/docs#issues' },
      { label: 'Cycles and roadmap', href: '/replicas/docs#cycles' },
      { label: 'Agent workflows', href: '/replicas/docs#agents' },
    ],
  },
  {
    heading: 'Reference',
    items: [
      { label: 'API', href: '/replicas/docs#api' },
      { label: 'Webhooks', href: '/replicas/docs#webhooks' },
      { label: 'Keyboard shortcuts', href: '/replicas/docs#shortcuts' },
    ],
  },
]

export const docsToc = [
  { label: 'Introduction', href: '#introduction' },
  { label: 'Quickstart', href: '#quickstart' },
  { label: 'Core concepts', href: '#concepts' },
  { label: 'Keyboard shortcuts', href: '#shortcuts' },
]

export const featureSections = [
  {
    eyebrow: 'Intake and integrations',
    title: 'Turn conversations into actionable work',
    description:
      'Automatically turn conversations and customer feedback into actionable issues that are routed, labeled, and prioritized for the right team.',
    href: '/replicas/features',
  },
  {
    eyebrow: 'Planning and monitoring',
    title: 'Plan and navigate from idea to launch',
    description:
      'Align your team with initiatives, roadmaps, and clear, up-to-date specifications that stay close to the work.',
    href: '/replicas/features',
    reverse: true,
  },
  {
    eyebrow: 'AI and automations',
    title: 'Build and deploy agents that work alongside you',
    description:
      'Work on complex tasks together or delegate entire issues end-to-end, with an activity trail you can review.',
    href: '/replicas/features',
  },
  {
    eyebrow: 'Build, review, and ship',
    title: 'Streamline code reviews with clear diffs',
    description:
      'Keep changes moving with better context and fewer back-and-forth comments, without sacrificing quality.',
    href: '/replicas/features',
    reverse: true,
  },
]

export const sampleCode = `import { createSignal } from 'solid-js'
import { Tabs } from 'web-design-system/components'

export function Settings() {
  const [open, setOpen] = createSignal(false)

  return (
    <Tabs
      tabs={[
        { value: 'general', label: 'General' },
        { value: 'members', label: 'Members' },
      ]}
    />
  )
}
`

export const diffBefore = [
  { content: "import { View, ActivityIndicator } from 'react-native'", kind: 'context' as const },
  { content: 'const { vehicleState, isFullySynced } = useVehicleState()', kind: 'removed' as const },
  { content: 'if (!isFullySynced) {', kind: 'removed' as const },
  { content: "  return <ActivityIndicator size='large' />", kind: 'removed' as const },
  { content: '}', kind: 'removed' as const },
]

export const diffAfter = [
  { content: "import { View, ActivityIndicator } from 'react-native'", kind: 'context' as const },
  { content: 'const { vehicleState, syncStatus } = useVehicleState()', kind: 'added' as const },
  { content: 'if (syncStatus === SyncStatus.PENDING) {', kind: 'added' as const },
  { content: "  return <ActivityIndicator size='large' />", kind: 'added' as const },
  { content: '}', kind: 'added' as const },
]
