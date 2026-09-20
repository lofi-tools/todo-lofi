/**
 * The documentation map. Two levels of navigation: the header's tab bar is the
 * two guides (one per audience), and each guide's left nav nests its pages
 * under section headings. One source of truth for the tabs, the left nav, and
 * the previous/next links, so a new page is one entry here and one file under
 * `src/pages/docs`.
 *
 * Every docs route lives under `/docs`; `/` is the landing page and belongs to
 * no guide.
 */

export interface DocLink {
  label: string
  href: string
}

export interface DocNavItem extends DocLink {
  active?: boolean
}

export interface DocSection {
  heading: string
  items: DocLink[]
}

export interface DocGuide {
  /** Tab label. */
  heading: string
  /** One line telling the reader whether this guide is for them. */
  summary: string
  /** The guide's landing page — also where its tab points. */
  href: string
  sections: DocSection[]
}

export const REPO_URL = 'https://github.com/lofi-tools/todo-lofi'

/** Where the docs start. The landing page and the header's brand link use it. */
export const DOCS_ROOT = '/docs'

export const docGuides: DocGuide[] = [
  {
    heading: 'User guide',
    summary: 'Install the app, use it, and configure it.',
    href: '/docs',
    sections: [
      {
        heading: 'Getting started',
        items: [
          { label: 'Overview', href: '/docs' },
          { label: 'Install and run', href: '/docs/install' },
          { label: 'Feature status', href: '/docs/status' },
        ],
      },
      {
        heading: 'Tasklist',
        items: [
          { label: 'Tasks, tags, and projects', href: '/docs/tasks' },
          { label: 'Repeat and scheduling', href: '/docs/repeats' },
          { label: 'Semi-automated workflows', href: '/docs/workflows' },
        ],
      },
      {
        heading: 'Integrations',
        items: [
          { label: 'Integrated AI agent', href: '/docs/agent' },
          { label: 'Two-way sync', href: '/docs/sync' },
          { label: 'Mini-apps and extensions', href: '/docs/mini-apps' },
        ],
      },
      {
        heading: 'Configuration',
        items: [
          { label: 'Configuration', href: '/docs/configuration' },
          { label: 'Glossary', href: '/docs/glossary' },
        ],
      },
    ],
  },
  {
    // The contributor guide is one flat list, so its single section heading is
    // the guide name itself.
    heading: 'Contributor guide',
    summary: 'How it works, and how to work on it.',
    href: '/docs/contributor',
    sections: [
      {
        heading: 'Contributor guide',
        items: [
          { label: 'Overview', href: '/docs/contributor' },
          { label: 'Development setup', href: '/docs/contributor/development' },
          { label: 'Architecture', href: '/docs/contributor/architecture' },
          { label: 'Data model', href: '/docs/contributor/data-model' },
          { label: 'Workflow engine', href: '/docs/contributor/workflow-engine' },
          { label: 'Agent runtime', href: '/docs/contributor/agent-runtime' },
          { label: 'Integrations', href: '/docs/contributor/integrations' },
          { label: 'Contributing', href: '/docs/contributor/contributing' },
          { label: 'License', href: '/docs/contributor/license' },
        ],
      },
    ],
  },
]

/** The guide a route belongs to. */
export function guideFor(pathname: string): DocGuide | undefined {
  const current = currentPath(pathname)
  return docGuides.find((guide) =>
    guide.sections.some((section) => section.items.some((item) => item.href === current)),
  )
}

/** The current guide's sections, with the active route marked. */
export function guideSections(pathname: string): DocSection[] {
  const current = currentPath(pathname)
  const guide = guideFor(pathname)
  if (!guide) return []
  return guide.sections.map((section) => ({
    heading: section.heading,
    items: section.items.map((item) => ({ ...item, active: item.href === current })),
  }))
}

/** Every page in a guide, in reading order, regardless of its section. */
export function guideLinks(guide: DocGuide): DocLink[] {
  return guide.sections.flatMap((section) => section.items)
}

/** The current guide's pages in reading order, with the active route marked. */
export function guideItems(pathname: string): DocNavItem[] {
  return guideSections(pathname).flatMap((section) => section.items)
}

/** The neighbours of the current route, within its guide. */
export function pageNeighbours(pathname: string): { previous?: DocLink; next?: DocLink } {
  const items = guideItems(pathname)
  const current = currentPath(pathname)
  const index = items.findIndex((item) => item.href === current)
  if (index === -1) return {}
  return { previous: items[index - 1], next: items[index + 1] }
}

/** Where the current route's source page lives in the repository. */
export function pageSourceUrl(pathname: string): string {
  const current = currentPath(pathname)
  // A guide's landing page is the index of its directory.
  const isLanding = docGuides.some((guide) => guide.href === current)
  const file = isLanding ? `${current}/index` : current
  return `${REPO_URL}/blob/main/apps/docs-web/src/pages${file}.astro`
}

function currentPath(pathname: string): string {
  return pathname.replace(/\/+$/, '') || '/'
}
