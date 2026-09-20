import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

interface Section {
  heading: string
  pages: [label: string, path: string][]
}

interface Guide {
  heading: string
  href: string
  sections: Section[]
}

/** Mirrors `src/data/docs.ts`: the tabs, and each guide's headed sections. */
const guides: Guide[] = [
  {
    heading: 'User guide',
    href: '/docs',
    sections: [
      {
        heading: 'Getting started',
        pages: [
          ['Overview', '/docs'],
          ['Install and run', '/docs/install'],
          ['Feature status', '/docs/status'],
        ],
      },
      {
        heading: 'Tasklist',
        pages: [
          ['Tasks, tags, and projects', '/docs/tasks'],
          ['Repeat and scheduling', '/docs/repeats'],
          ['Semi-automated workflows', '/docs/workflows'],
        ],
      },
      {
        heading: 'Integrations',
        pages: [
          ['Integrated AI agent', '/docs/agent'],
          ['Two-way sync', '/docs/sync'],
          ['Mini-apps and extensions', '/docs/mini-apps'],
        ],
      },
      {
        heading: 'Configuration',
        pages: [
          ['Configuration', '/docs/configuration'],
          ['Glossary', '/docs/glossary'],
        ],
      },
    ],
  },
  {
    heading: 'Contributor guide',
    href: '/docs/contributor',
    sections: [
      {
        heading: 'Contributor guide',
        pages: [
          ['Overview', '/docs/contributor'],
          ['Development setup', '/docs/contributor/development'],
          ['Architecture', '/docs/contributor/architecture'],
          ['Data model', '/docs/contributor/data-model'],
          ['Workflow engine', '/docs/contributor/workflow-engine'],
          ['Agent runtime', '/docs/contributor/agent-runtime'],
          ['Integrations', '/docs/contributor/integrations'],
          ['Contributing', '/docs/contributor/contributing'],
          ['License', '/docs/contributor/license'],
        ],
      },
    ],
  },
]

const pagesOf = (guide: Guide) => guide.sections.flatMap((section) => section.pages)
const docsRoutes = guides.flatMap(pagesOf)
/** The landing page is not part of either guide, but is still a page we ship. */
const routes: [label: string, path: string][] = [['Home', '/'], ...docsRoutes]

test.describe('docs routes', () => {
  // Both guides have a page called Overview, so the guide is part of the title.
  for (const guide of guides) {
    for (const [name, path] of pagesOf(guide)) {
      test(`${guide.heading} · ${name} renders without console errors`, async ({ page }) => {
        const errors: string[] = []
        page.on('console', (message) => {
          if (message.type() === 'error') errors.push(message.text())
        })
        page.on('pageerror', (error) => errors.push(error.message))

        const response = await page.goto(path)
        expect(response?.ok()).toBeTruthy()
        await expect(page.locator('h1').first()).toBeVisible()
        expect(errors).toEqual([])
      })
    }
  }
})

test('every documented page lives under /docs', () => {
  for (const [, path] of docsRoutes) expect(path.startsWith('/docs')).toBe(true)
})

test.describe('landing page', () => {
  test('renders without console errors and marks / as current', async ({ page }) => {
    const errors: string[] = []
    page.on('console', (message) => {
      if (message.type() === 'error') errors.push(message.text())
    })
    page.on('pageerror', (error) => errors.push(error.message))

    const response = await page.goto('/')
    expect(response?.ok()).toBeTruthy()
    await expect(page.locator('h1').first()).toBeVisible()
    await expect(page.locator('header nav[aria-label="Main"] a[aria-current="page"]')).toHaveText(
      /todo-lofi/,
    )
    expect(errors).toEqual([])
  })

  test('links into both guides, and every docs route is reachable from it', async ({ page }) => {
    const main = page.locator('main')
    await page.goto('/')

    await expect(main.getByRole('link', { name: /read the docs/i }).first()).toBeVisible()
    for (const [, path] of docsRoutes) {
      await expect(
        page.locator(`a[href="${path}"]`).first(),
        `${path} should be linked from the landing page`,
      ).toHaveCount(1)
    }

    await main.getByRole('link', { name: /read the docs/i }).first().click()
    await expect(page).toHaveURL(/\/docs\/?$/)
    await expect(page.locator('h1').first()).toHaveText('Overview')
  })

  // The automations pane is a replica, not a bitmap: it has to be built and
  // styled on both pages that illustrate with it.
  for (const [where, path] of [
    ['landing page', '/'],
    ['overview', '/docs'],
  ] as const) {
    test(`the automations pane illustration renders on the ${where}`, async ({ page }) => {
      await page.goto(path)

      await expect(page.getByText('Branches to clean up')).toBeVisible()

      const badge = page.getByText('Round 2 · Implement')
      await expect(badge).toBeVisible()
      // Present is not enough: the design system's recipe class has to have
      // reached it, which shows up as the tone's tinted background.
      const background = await badge.evaluate((node) => getComputedStyle(node).backgroundColor)
      expect(background).not.toBe('rgba(0, 0, 0, 0)')
    })
  }
})

test('the design system stylesheet is applied', async ({ page }) => {
  await page.goto('/docs')
  const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor)
  // Must not fall back to the browser default (transparent).
  expect(background).not.toBe('rgba(0, 0, 0, 0)')
  expect(background).not.toBe('')

  const token = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue('--colors-accent').trim(),
  )
  expect(token.length).toBeGreaterThan(0)
})

test('theme toggle switches to light and persists', async ({ page }) => {
  await page.goto('/docs')
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')

  await page.getByRole('button', { name: /switch to light theme/i }).first().click()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')

  const stored = await page.evaluate(() => localStorage.getItem('wds-theme'))
  expect(stored).toBe('light')
})

test('the header has one tab per guide and no page-level entries', async ({ page }) => {
  const header = page.locator('header nav[aria-label="Main"]')

  await page.goto('/docs/tasks')
  await expect(header.getByRole('link', { name: 'User guide' })).toHaveAttribute(
    'aria-current',
    'true',
  )
  await expect(header.getByRole('link', { name: 'Contributor guide' })).not.toHaveAttribute(
    'aria-current',
    'true',
  )
  await expect(header.getByRole('link', { name: 'Configuration' })).toHaveCount(0)

  await page.goto('/docs/contributor/architecture')
  await expect(header.getByRole('link', { name: 'Contributor guide' })).toHaveAttribute(
    'aria-current',
    'true',
  )
})

test('the left nav nests pages under section headings, scoped to the guide', async ({ page }) => {
  const sidebar = page.locator('nav[aria-label="Documentation"]')

  for (const guide of guides) {
    await page.goto(guide.href)
    await expect(sidebar.locator('h2')).toHaveText(guide.sections.map((section) => section.heading))
    await expect(sidebar.locator('a')).toHaveText(pagesOf(guide).map(([label]) => label))


    // No leakage from the other guide, and the landing page is marked active.
    await expect(sidebar.locator('a', { hasText: 'License' })).toHaveCount(
      guide.heading === 'Contributor guide' ? 1 : 0,
    )
    await expect(sidebar.locator('a[aria-current="page"]')).toHaveText(
      pagesOf(guide)[0][0],
    )
  }
})

test('the pager moves within a guide, across sections', async ({ page }) => {
  await page.goto('/docs/status')
  await expect(page.locator('nav[aria-label="Documentation"] a[aria-current="page"]')).toHaveText(
    'Feature status',
  )

  // Last page of "Getting started" → first page of "Tasklist".
  await page.getByRole('link', { name: /next/i }).first().click()
  await expect(page).toHaveURL(/\/docs\/tasks\/?$/)
  await expect(page.locator('h1').first()).toHaveText('Tasks, tags, and projects')
})

test('every page in a guide is reachable from its left nav', async ({ page }) => {
  const sidebar = page.locator('nav[aria-label="Documentation"]')
  for (const guide of guides) {
    await page.goto(guide.href)
    for (const [, path] of pagesOf(guide)) {
      await sidebar.locator(`a[href="${path}"]`).first().click()
      await expect(page.locator('h1').first()).toBeVisible()
    }
  }
})

test('the layout holds from mobile to desktop without horizontal overflow', async ({ page }) => {
  for (const width of [375, 768, 1440]) {
    await page.setViewportSize({ width, height: 900 })
    for (const [, path] of routes) {
      await page.goto(path)
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      )
      expect(overflow, `${path} at ${width}px wide`).toBeLessThanOrEqual(1)
    }
  }
})

test('narrow viewports get guide tabs and the left nav collapses', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto('/docs/contributor/development')
  await expect(page.locator('nav[aria-label="Documentation"]')).toBeVisible()
  await expect(page.locator('nav[aria-label="On this page"]')).toBeVisible()
  await expect(page.locator('nav[aria-label="Guides"]')).toBeHidden()

  await page.setViewportSize({ width: 375, height: 812 })
  await expect(page.locator('nav[aria-label="Documentation"]')).toBeHidden()
  await expect(page.locator('nav[aria-label="Guides"]')).toBeVisible()

  // The compact nav still separates guides from pages, and only lists this one.
  const sections = page.locator('nav[aria-label="Docs sections"]')
  await expect(sections.locator('a')).toHaveText(pagesOf(guides[1]).map(([label]) => label))
})

test('accessibility: WCAG 2.1 AA on every route', async ({ page }) => {
  for (const [, path] of routes) {
    await page.goto(path)
    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
      .analyze()
    const serious = results.violations.filter(
      (violation) => violation.impact === 'serious' || violation.impact === 'critical',
    )
    expect(
      serious,
      `${path}: ${serious.map((violation) => `${violation.id} (${violation.nodes.length})`).join(', ')}`,
    ).toEqual([])
  }
})
