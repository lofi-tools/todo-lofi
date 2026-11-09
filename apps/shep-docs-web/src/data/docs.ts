/**
 * The Shepherd documentation map. The header's tabs are the two audience
 * guides; each guide's left nav, the footer and the previous/next links all
 * read from this file. A page is one entry here and one file under
 * `src/pages/docs`.
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
  heading: string
  summary: string
  href: string
  sections: DocSection[]
}

import { REPO_URL } from '../../data/shepherd'
import { ROADMAP_PATH } from '../../data/roadmap'

export { REPO_URL, ROADMAP_PATH }

export const DOCS_ROOT = '/docs/users'

export const docGuides: DocGuide[] = [
  {
    heading: 'User guide',
    summary: 'Install Shepherd, run agents, and use the workspace.',
    href: '/docs/users',
    sections: [
      {
        heading: 'Getting started',
        items: [
          { label: 'Overview', href: '/docs/users' },
          { label: 'Install and run', href: '/docs/users/install' },
          { label: 'Configuration', href: '/docs/users/configuration' },
        ],
      },
      {
        heading: 'The workspace',
        items: [
          { label: 'Mental model', href: '/docs/users/mental-model' },
          { label: 'Preview and annotation', href: '/docs/users/preview' },
          { label: 'Agent pool', href: '/docs/users/agent-pool' },
        ],
      },
      {
        heading: 'Operations',
        items: [
          { label: 'Roadmap', href: ROADMAP_PATH },
          { label: 'Releases', href: '/docs/users/releases' },
          { label: 'Troubleshooting', href: '/docs/users/troubleshooting' },
        ],
      },
    ],
  },
  {
    heading: 'Contributor guide',
    summary: 'Work on the core, adapters, preview API and release pipeline.',
    href: '/docs/contributors',
    sections: [
      {
        heading: 'Contributor guide',
        items: [
          { label: 'Overview', href: '/docs/contributors' },
          { label: 'Adapter contracts', href: '/docs/contributors/adapter-contracts' },
          { label: 'Annotation API', href: '/docs/contributors/annotation-api' },
          { label: 'Quota ledger', href: '/docs/contributors/quota-ledger' },
          { label: 'Release pipeline', href: '/docs/contributors/release-pipeline' },
          { label: 'Test fixtures', href: '/docs/contributors/test-fixtures' },
        ],
      },
    ],
  },
]

/** The docs links that repeat in the site footer. */
export const footerDocLinks: DocLink[] = [
  { label: 'User guide', href: '/docs/users' },
  { label: 'Roadmap', href: ROADMAP_PATH },
  { label: 'Install and run', href: '/docs/users/install' },
  { label: 'Preview and annotation', href: '/docs/users/preview' },
  { label: 'Contributor guide', href: '/docs/contributors' },
  { label: 'Adapter contracts', href: '/docs/contributors/adapter-contracts' },
  { label: 'Release pipeline', href: '/docs/contributors/release-pipeline' },
]

export function guideFor(pathname: string): DocGuide | undefined {
  const current = currentPath(pathname)
  return docGuides.find((guide) =>
    guide.sections.some((section) => section.items.some((item) => item.href === current)),
  )
}

export function guideSections(pathname: string): DocSection[] {
  const current = currentPath(pathname)
  const guide = guideFor(pathname)
  if (!guide) return []
  return guide.sections.map((section) => ({
    heading: section.heading,
    items: section.items.map((item) => ({ ...item, active: item.href === current })),
  }))
}

export function guideLinks(guide: DocGuide): DocLink[] {
  return guide.sections.flatMap((section) => section.items)
}

export function guideItems(pathname: string): DocNavItem[] {
  return guideSections(pathname).flatMap((section) => section.items)
}

export function pageNeighbours(pathname: string): { previous?: DocLink; next?: DocLink } {
  const items = guideItems(pathname)
  const current = currentPath(pathname)
  const index = items.findIndex((item) => item.href === current)
  if (index === -1) return {}
  return { previous: items[index - 1], next: items[index + 1] }
}

export function pageSourceUrl(pathname: string): string {
  const current = currentPath(pathname)
  const isLanding = docGuides.some((guide) => guide.href === current)
  const file = isLanding ? `${current}/index` : current
  return `${REPO_URL}/blob/main/apps/shep-docs-web/src/pages${file}.astro`
}

function currentPath(pathname: string): string {
  return pathname.replace(/\/+$/, '') || '/'
}
