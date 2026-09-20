/**
 * The documentation map. One source of truth for the header nav, the sidebar,
 * and the previous/next links, so a new page is one entry here plus one file
 * in `src/pages`.
 */

export interface DocLink {
  label: string
  href: string
}

export interface DocGroup {
  heading: string
  items: DocLink[]
}

export const REPO_URL = 'https://github.com/lofi-tools/todo-lofi'

export const docGroups: DocGroup[] = [
  {
    heading: 'Getting started',
    items: [
      { label: 'Overview', href: '/' },
      { label: 'Quickstart', href: '/quickstart' },
    ],
  },
  {
    heading: 'Concepts',
    items: [
      { label: 'Features', href: '/features' },
      { label: 'Architecture', href: '/architecture' },
      { label: 'Integrations', href: '/integrations' },
    ],
  },
  {
    heading: 'Project',
    items: [
      { label: 'Contributing', href: '/contributing' },
      { label: 'License', href: '/license' },
    ],
  },
]

/** Flat reading order, used for the previous/next footer links. */
export const docPages: DocLink[] = docGroups.flatMap((group) => group.items)

/** Sidebar groups with the current route marked active. */
export function sidebarGroups(pathname: string): DocGroup[] {
  const current = pathname.replace(/\/+$/, '') || '/'
  return docGroups.map((group) => ({
    heading: group.heading,
    items: group.items.map((item) => ({
      ...item,
      active: item.href === current,
    })),
  }))
}

/** Where the current route's source page lives in the repository. */
export function pageSourceUrl(pathname: string): string {
  const slug = pathname.replace(/^\/+|\/+$/g, '')
  return `${REPO_URL}/blob/main/apps/docs-web/src/pages/${slug === '' ? 'index' : slug}.astro`
}

/** The neighbours of the current route in reading order. */
export function pageNeighbours(pathname: string): { previous?: DocLink; next?: DocLink } {
  const current = pathname.replace(/\/+$/, '') || '/'
  const index = docPages.findIndex((page) => page.href === current)
  if (index === -1) return {}
  return { previous: docPages[index - 1], next: docPages[index + 1] }
}
