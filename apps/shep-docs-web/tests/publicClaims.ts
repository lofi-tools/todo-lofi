import { expect, type Page } from '@playwright/test'
import { expectNoLegacyPublicLinks } from 'web-test-guards/publicLinks'
import { ROADMAP_PATH, roadmapItems, roadmapStatuses } from '../data/roadmap'

interface ClaimEntry {
  itemId: string
  itemTitle: string
  term: string
}

interface ClaimContext extends ClaimEntry {
  clause: string
  scope: string
}

const claimItems = roadmapItems.filter((item) => item.publicClaimTerms?.length)
const claimEntries: ClaimEntry[] = claimItems.flatMap((item) =>
  (item.publicClaimTerms ?? []).map((term) => ({
    itemId: item.id,
    itemTitle: item.title,
    term,
  })),
)

const unshippedStatuses = roadmapStatuses.filter((status) => !status.shipped)
const unshippedMarkers = unique(
  unshippedStatuses.flatMap((status) => [
    status.label,
    status.badge,
    status.landingHeading,
    status.landingBadge,
    ...status.claimMarkers,
  ]),
)

const shippedStatuses = roadmapStatuses.filter((status) => status.shipped)
const shippedMarkers = unique([
  ...shippedStatuses.flatMap((status) => [
    status.badge,
    status.landingHeading,
    status.landingBadge,
    ...status.claimMarkers,
  ]),
  'available',
  'shipped',
  'released',
  'supported',
  'ready',
  'works',
])

function unique(values: Array<string | undefined>): string[] {
  return [...new Set(values.filter((value): value is string => Boolean(value)))]
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function assertClaimData(): void {
  expect(claimEntries.length, 'the roadmap must define public claim terms').toBeGreaterThan(0)

  for (const item of claimItems) {
    const status = roadmapStatuses.find((entry) => entry.id === item.status)
    expect(status, `${item.id} has no roadmap status`).toBeDefined()
    expect(
      status?.shipped,
      `${item.id} is marked shipped; a restricted public claim must stay next/later`,
    ).toBe(false)
  }
}

function positiveClaimIn(text: string): string | undefined {
  const lower = text.toLowerCase()

  for (const marker of shippedMarkers) {
    const pattern = new RegExp(`\\b${escapeRegex(marker.toLowerCase())}\\b`, 'g')
    let match: RegExpExecArray | null

    while ((match = pattern.exec(lower))) {
      const before = lower.slice(Math.max(0, match.index - 48), match.index)

      // "not available yet", "no supported build" and "not shipped" are
      // negative claims, not evidence that the feature is available.
      if (/\b(?:not|no|never|without|none|neither|nor)\b[^.!?]*$/.test(before)) continue
      // A future or planned claim is still not available today.
      if (/\b(?:will|would|should|could|may|might)\s+(?:be\s+)?$/.test(before)) continue
      if (/\b(?:planned|expected|intended|coming)\s+to\s+be\s+$/.test(before)) continue

      return marker
    }
  }

  return undefined
}

function hasUnshippedMarker(context: ClaimContext): boolean {
  const text = `${context.clause}\n${context.scope}`.toLowerCase()
  return unshippedMarkers.some((marker) => text.includes(marker.toLowerCase()))
}

async function collectClaimContexts(page: Page, path: string): Promise<ClaimContext[]> {
  const root = path === '/' ? 'main' : 'main article'

  return page.locator(root).evaluate((element, entries: ClaimEntry[]) => {
    const escape = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    const blocks = Array.from(element.querySelectorAll('h1, h2, h3, h4, p, li, tr, figcaption'))
    const found: ClaimContext[] = []

    for (const block of blocks) {
      const text = (block as HTMLElement).innerText.trim()
      if (!text) continue

      for (const entry of entries) {
        const pattern = new RegExp(`\\b${escape(entry.term)}\\b`, 'i')
        if (!pattern.test(text)) continue

        const clauses = text
          .split(/\n+|·|(?<=[.!?])\s+/)
          .map((clause) => clause.trim())
          .filter(Boolean)

        for (const clause of clauses) {
          if (!pattern.test(clause)) continue

          const scope =
            block.closest('section, tr, figure, [role="note"], aside, article') ?? block
          found.push({
            ...entry,
            clause,
            scope: (scope as HTMLElement).innerText.trim(),
          })
        }
      }
    }

    return found
  }, claimEntries)
}

export async function expectPublicClaims(page: Page, path: string): Promise<void> {
  assertClaimData()

  const contexts = await collectClaimContexts(page, path)

  if (path === ROADMAP_PATH) {
    const foundItems = [...new Set(contexts.map((context) => context.itemId))].sort()
    expect(
      foundItems,
      `${path} must exercise every restricted public claim`,
    ).toEqual(claimItems.map((item) => item.id).sort())
  }

  const claimItemById = new Map(claimItems.map((item) => [item.id, item]))
  const statusById = new Map(roadmapStatuses.map((status) => [status.id, status]))
  const seen = new Set<string>()

  for (const context of contexts) {
    const key = `${context.itemId}|${context.term}|${context.clause}`
    if (seen.has(key)) continue
    seen.add(key)

    const item = claimItemById.get(context.itemId)
    const status = item ? statusById.get(item.status) : undefined
    expect(status, `${context.itemId} has no roadmap status`).toBeDefined()

    const positive = positiveClaimIn(context.clause)
    expect(
      positive,
      `${path}: "${context.clause}" presents ${context.itemTitle} as ${positive ?? 'available'}; the roadmap marks it ${status?.label ?? 'unshipped'}`,
    ).toBeUndefined()

    expect(
      hasUnshippedMarker(context),
      `${path}: "${context.clause}" mentions ${context.itemTitle} without a Next/Later/planned marker`,
    ).toBe(true)
  }

  await expectNoLegacyPublicLinks(page, path)
}
