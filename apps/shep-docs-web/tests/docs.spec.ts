import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

const REPO_URL = 'https://github.com/lofi-tools/todo-lofi'

const docsRoutes = [
  { path: '/docs/users', title: 'Overview', guide: 'User guide' },
  { path: '/docs/users/install', title: 'Install and run', guide: 'User guide' },
  { path: '/docs/users/configuration', title: 'Configuration', guide: 'User guide' },
  { path: '/docs/users/mental-model', title: 'Mental model', guide: 'User guide' },
  { path: '/docs/users/preview', title: 'Preview and annotation', guide: 'User guide' },
  { path: '/docs/users/agent-pool', title: 'Agent pool', guide: 'User guide' },
  { path: '/docs/users/roadmap', title: 'Roadmap', guide: 'User guide' },
  { path: '/docs/users/releases', title: 'Releases', guide: 'User guide' },
  { path: '/docs/users/troubleshooting', title: 'Troubleshooting', guide: 'User guide' },
  { path: '/docs/contributors', title: 'Contributor guide', guide: 'Contributor guide' },
  { path: '/docs/contributors/adapter-contracts', title: 'Adapter contracts', guide: 'Contributor guide' },
  { path: '/docs/contributors/annotation-api', title: 'Annotation API', guide: 'Contributor guide' },
  { path: '/docs/contributors/quota-ledger', title: 'Quota ledger', guide: 'Contributor guide' },
  { path: '/docs/contributors/release-pipeline', title: 'Release pipeline', guide: 'Contributor guide' },
  { path: '/docs/contributors/test-fixtures', title: 'Test fixtures', guide: 'Contributor guide' },
] as const

function sourcePath(path: string) {
  return path === '/docs/users' || path === '/docs/contributors' ? `${path}/index` : path
}

test.describe('Shepherd docs routes', () => {
  for (const route of docsRoutes) {
    test(`${route.path} renders in the ${route.guide}`, async ({ page }) => {
      const errors: string[] = []
      page.on('console', (message) => {
        if (message.type() === 'error') errors.push(message.text())
      })
      page.on('pageerror', (error) => errors.push(error.message))

      const response = await page.goto(route.path)
      expect(response?.status()).toBe(200)
      await expect(page.locator('h1').first()).toHaveText(route.title)
      await expect(page.locator('header nav[aria-label="Main"] a[aria-current="true"]')).toHaveText(
        route.guide,
      )
      await expect(page.locator(`main aside a[href="${route.path}"]`).first()).toBeVisible()

      const edit = page.getByRole('link', { name: 'Edit this page on GitHub' })
      await expect(edit).toHaveAttribute(
        'href',
        `${REPO_URL}/blob/main/apps/shep-docs-web/src/pages${sourcePath(route.path)}.astro`,
      )

      expect(errors).toEqual([])
    })
  }

  test('the guide tabs switch between the two audiences', async ({ page }) => {
    await page.goto('/docs/users')
    const tabs = page.locator('header nav[aria-label="Main"]')
    await tabs.getByRole('link', { name: 'Contributor guide' }).click()
    await expect(page).toHaveURL(/\/docs\/contributors$/)
    await expect(page.locator('h1')).toHaveText('Contributor guide')

    await tabs.getByRole('link', { name: 'User guide' }).click()
    await expect(page).toHaveURL(/\/docs\/users$/)
    await expect(page.locator('h1')).toHaveText('Overview')
  })

  test('the mobile guide switcher works below the header breakpoint', async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 812 })
    await page.goto('/docs/users/preview')

    const guides = page.locator('nav[aria-label="Guides"]')
    await expect(guides).toBeVisible()
    await guides.getByRole('link', { name: 'Contributor guide' }).click()
    await expect(page).toHaveURL(/\/docs\/contributors$/)
    await expect(page.locator('h1')).toHaveText('Contributor guide')
  })

  test('every route is listed in its guide navigation', async ({ page }) => {
    for (const guide of ['/docs/users', '/docs/contributors'] as const) {
      await page.goto(guide)
      const links = page.locator('main aside a[href^="/docs/"]')
      const hrefs = await links.evaluateAll((elements) =>
        elements.map((element) => element.getAttribute('href')),
      )
      for (const route of docsRoutes.filter((entry) => entry.path.startsWith(guide))) {
        expect(hrefs, `${route.path} is missing from ${guide}`).toContain(route.path)
      }
    }
  })

  test('the agent pool, roadmap and releases are described honestly', async ({ page }) => {
    await page.goto('/docs/users/agent-pool')
    await expect(page.locator('main')).toContainText(/not in the current build/i)
    await expect(page.locator('main')).not.toContainText(/available now/i)

    await page.goto('/docs/users/roadmap')
    await expect(page.locator('main')).toContainText(/only now is shipped/i)
    await expect(page.locator('#next')).toContainText('Quota-aware agent pool')
    await expect(page.locator('#next')).toContainText('Signed macOS and Linux release artifacts')
    await expect(page.locator('#next')).toContainText('not shipped')
    await expect(page.locator('#later')).toContainText('Windows desktop support')
    await expect(page.locator('#later')).toContainText('planned')
    await expect(page.locator('#exploring')).toContainText('Agent teams and orchestration')

    await page.goto('/docs/users/releases')
    await expect(page.locator('main')).toContainText(/no signed release yet/i)
    await expect(page.locator('main')).toContainText(/release gate has not been run/i)
    await expect(page.locator('a[href="/downloads"], a[href="/roadmap"]')).toHaveCount(0)
  })

  test('no docs page names an internal reference project or Taskstream copy', async ({ page }) => {
    for (const route of docsRoutes) {
      await page.goto(route.path)
      const text = await page.locator('body').innerText()
      expect(text, route.path).not.toMatch(/taskstream/i)
      expect(text, route.path).not.toMatch(/\b(?:pandacode|herdr(?:-core|-app)?|pi|freebuff)\b/i)
    }
  })

  test('the docs layout holds from mobile to desktop without horizontal overflow', async ({ page }) => {
    for (const width of [375, 768, 1440]) {
      await page.setViewportSize({ width, height: 900 })
      for (const route of docsRoutes) {
        await page.goto(route.path)
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
        )
        expect(overflow, `${route.path} at ${width}px wide`).toBeLessThanOrEqual(1)
      }
    }
  })

  test('accessibility: WCAG 2.1 AA on every docs route', async ({ page }) => {
    for (const route of docsRoutes) {
      await page.goto(route.path)
      const results = await new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
        .analyze()
      const serious = results.violations.filter(
        (violation) => violation.impact === 'serious' || violation.impact === 'critical',
      )
      expect(
        serious,
        `${route.path}: ${serious
          .map((violation) => `${violation.id} (${violation.nodes.length})`)
          .join(', ')}`,
      ).toEqual([])
    }
  })
})
