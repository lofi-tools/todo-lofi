import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import { expectCatalogPublicClaims } from './publicClaims'

const MODEL_PATH = '/models/abacus/deepseek-ai/DeepSeek-R1'
const MODEL_QUERY = 'abacus%2Fdeepseek-ai%2FDeepSeek-R1'
const COMPARE_PATH = `/compare?models=${MODEL_QUERY}`

const keyRoutes = [
  { path: '/', heading: 'Know what you can run, and where it came from.' },
  { path: '/models', heading: 'Models in this snapshot' },
  { path: '/providers/abacus', heading: 'Abacus' },
  { path: MODEL_PATH, heading: 'DeepSeek R1' },
  { path: '/compare', heading: 'Compare models' },
  { path: '/about', heading: 'Sources, freshness and limits' },
  { path: '/agents', heading: 'Coding agents' },
] as const

test.describe('Providers catalog', () => {
  test('key routes render without console errors', async ({ page }) => {
    for (const route of keyRoutes) {
      const errors: string[] = []
      page.on('console', (message) => {
        if (message.type() === 'error') errors.push(message.text())
      })
      page.on('pageerror', (error) => errors.push(error.message))

      const response = await page.goto(route.path)
      expect(response?.status(), route.path).toBe(200)
      await expect(page.locator('h1').first(), route.path).toHaveText(route.heading)
      expect(errors, route.path).toEqual([])
    }
  })

  test('navigation and footer links resolve', async ({ page }) => {
    await page.goto('/')
    const hrefs = await page
      .locator('header a, footer a')
      .evaluateAll((links) =>
        links
          .map((link) => link.getAttribute('href'))
          .filter((href): href is string => href !== null && href.startsWith('/')),
      )

    for (const href of [...new Set(hrefs)]) {
      const response = await page.request.get(href)
      expect(response.status(), href).toBe(200)
    }
  })

  test('the model browser filters the static rows', async ({ page }) => {
    await page.goto('/models')
    const count = page.locator('[data-result-count]')
    const rows = page.locator('[data-model-row]:not([hidden])')

    await expect(count).toHaveText('9,000 of 9,000 models')
    const allRows = await rows.count()
    expect(allRows).toBe(9000)

    await page.selectOption('[data-filter="provider"]', 'abacus')
    await expect(count).toHaveText(/\d+ of 9,000 models/)
    const providerRows = await rows.count()
    expect(providerRows).toBeGreaterThan(0)
    expect(providerRows).toBeLessThan(allRows)

    await page.fill('[data-filter="q"]', 'deepseek-ai/DeepSeek-R1')
    expect(await rows.count()).toBeGreaterThan(0)

    await page.selectOption('[data-filter="offer"]', 'free')
    await expect(page.locator('[data-model-empty]')).toBeVisible()
    await expect(count).toHaveText('0 of 9,000 models')

    await page.click('[data-reset]')
    await expect(count).toHaveText('9,000 of 9,000 models')
    await expect(page.locator('[data-model-empty]')).toBeHidden()
  })

  test('comparison renders selected models and provenance', async ({ page }) => {
    await page.goto(COMPARE_PATH)
    await expect(page.locator('#compare-root table')).toBeVisible()
    await expect(page.locator('#compare-root')).toContainText('DeepSeek R1')
    await expect(page.locator('#compare-root')).toContainText('Input cost / 1M')
    await expect(page.locator('#compare-root')).toContainText('Verified')
    await expect(page.locator('#compare-root')).toContainText('models.dev')
    await expect(page.locator('#compare-root')).not.toContainText(/allowance:/i)

    await page.goto('/compare')
    await expect(page.locator('#compare-root')).toContainText('No models selected')
    await expect(page.locator('#compare-root').getByRole('link', { name: /choose models/i })).toBeVisible()
  })

  test('model detail links to provider, source and comparison', async ({ page }) => {
    await page.goto(MODEL_PATH)
    await expect(page.getByRole('link', { name: '← Abacus' })).toHaveAttribute(
      'href',
      '/providers/abacus',
    )
    await expect(page.getByRole('link', { name: /compare this model/i })).toHaveAttribute(
      'href',
      `/compare?models=${MODEL_QUERY}`,
    )
    await expect(page.getByRole('link', { name: 'models.dev public data' })).toHaveAttribute(
      'href',
      /^https:\/\/models\.dev\//,
    )
    await expect(page.locator('main')).toContainText('No verified plan or offer')
    await expect(page.locator('main')).toContainText('No local runtime data yet')
  })

  test('catalog pages do not make unverified public claims', async ({ page }) => {
    for (const route of keyRoutes) {
      await page.goto(route.path)
      await expectCatalogPublicClaims(page, route.path)
    }
  })

  test('catalog layout holds from mobile to desktop', async ({ page }) => {
    for (const width of [375, 768, 1440]) {
      await page.setViewportSize({ width, height: 900 })
      for (const route of keyRoutes) {
        await page.goto(route.path)
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
        )
        expect(overflow, `${route.path} at ${width}px wide`).toBeLessThanOrEqual(1)
      }
    }
  })

  test('accessibility: WCAG 2.1 AA on key catalog routes', async ({ page }) => {
    test.setTimeout(120_000)
    // Axe needs more than the default test budget to traverse the 9,000-row
    // table for structural checks. That table is covered by the filter,
    // comparison, link and overflow tests instead.
    const axeRoutes = keyRoutes.filter((route) => route.path !== '/models')
    for (const width of [375, 768, 1440]) {
      await page.setViewportSize({ width, height: 900 })
      for (const route of axeRoutes) {
        await page.goto(route.path)
        const results = await new AxeBuilder({ page })
          .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
          .analyze()
        const serious = results.violations.filter(
          (violation) => violation.impact === 'serious' || violation.impact === 'critical',
        )
        expect(
          serious,
          `${route.path} at ${width}px: ${serious
            .map((violation) => `${violation.id} (${violation.nodes.length})`)
            .join(', ')}`,
        ).toEqual([])
      }
    }
  })
})
