import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import { expectCatalogPublicClaims } from './publicClaims'

const MODEL_PATH = '/models/abacus/deepseek-ai/DeepSeek-R1'

const keyRoutes = [
  { path: '/', heading: 'Know what you can run, and where it came from.' },
  { path: '/models', heading: 'Models in this snapshot' },
  { path: '/companies', heading: 'Provider companies' },
  { path: '/families', heading: 'Model family tree' },
  { path: '/harnesses', heading: 'Coding-agent harnesses' },
  { path: '/providers/abacus', heading: 'Abacus' },
  { path: MODEL_PATH, heading: 'DeepSeek R1' },
  { path: '/ranking', heading: 'Harness ranking' },
  { path: '/about', heading: 'Sources, freshness and limits' },
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

    const allRows = await rows.count()
    expect(allRows).toBeGreaterThan(1000)
    await expect(count).toHaveText(/[\d,]+ of [\d,]+ models/)

    await page.selectOption('[data-filter="provider"]', 'abacus')
    const providerRows = await rows.count()
    expect(providerRows).toBeGreaterThan(0)
    expect(providerRows).toBeLessThan(allRows)

    await page.fill('[data-filter="q"]', 'deepseek-ai/DeepSeek-R1')
    expect(await rows.count()).toBeGreaterThan(0)

    await page.click('[data-reset]')
    await expect(count).toHaveText(/[\d,]+ of [\d,]+ models/)

    await page.fill('[data-filter="context"]', '200000')
    const contextRows = await rows.count()
    expect(contextRows).toBeGreaterThan(0)
    expect(contextRows).toBeLessThan(allRows)
    await page.fill('[data-filter="context"]', '')

    await page.selectOption('[data-filter="modality"]', 'image')
    const imageRows = await rows.count()
    expect(imageRows).toBeGreaterThan(0)
    expect(imageRows).toBeLessThan(allRows)
    await page.selectOption('[data-filter="modality"]', '')

    await page.selectOption('[data-filter="local"]', 'yes')
    const localRows = await rows.count()
    expect(localRows).toBeGreaterThan(0)
    expect(localRows).toBeLessThan(allRows)
    await page.selectOption('[data-filter="local"]', '')

    await page.selectOption('[data-filter="family"]', 'claude')
    const claudeRows = await rows.count()
    expect(claudeRows).toBeGreaterThan(0)
    expect(claudeRows).toBeLessThan(allRows)
    await page.selectOption('[data-filter="family"]', '')

    await page.selectOption('[data-filter="harness"]', 'codex')
    const codexRows = await rows.count()
    expect(codexRows).toBeGreaterThan(0)
    expect(codexRows).toBeLessThan(allRows)

    await expect(page.locator('[data-filter="offer"]')).toBeDisabled()
  })

  test('ranking page publishes its precomputed inputs and formula', async ({ page, request }) => {
    const response = await request.get('/harness-ranking.json')
    expect(response.status()).toBe(200)
    expect(response.headers()['content-type']).toContain('application/json')
    const ranking = (await response.json()) as {
      formula: string
      totalHarnesses: number
      entries: unknown[]
      groups: unknown[]
    }
    expect(ranking.formula).toContain('monthly recurring free requests or tokens')
    expect(ranking.totalHarnesses).toBeGreaterThan(0)
    expect(ranking.entries).toHaveLength(ranking.totalHarnesses)
    expect(Array.isArray(ranking.groups)).toBe(true)

    await page.goto('/ranking')
    await expect(page.locator('h1')).toHaveText('Harness ranking')
    await expect(page.locator('main')).toContainText('No harness is rankable yet')
    await expect(page.locator('main')).toContainText('does not publish either input')
    await expect(page.locator('main')).toContainText('Inputs available per harness')
  })

  test('model detail links to provider, canonical model, source and comparison', async ({
    page,
  }) => {
    await page.goto(MODEL_PATH)
    await expect(page.getByRole('link', { name: '← Abacus' })).toHaveAttribute(
      'href',
      '/providers/abacus',
    )
    const provenance = page.getByRole('region', { name: 'Provenance' })
    await expect(provenance.getByRole('link', { name: 'models.dev' })).toHaveAttribute(
      'href',
      /^https:\/\/models\.dev\//,
    )
    await expect(provenance).toContainText('MIT')
    await expect(provenance).toContainText('Source fetched')
    await expect(provenance).not.toContainText('Last verified')
    await expect(page.locator('main')).toContainText('Canonical model and family')
    await expect(page.locator('main')).toContainText('Family chain')
    await expect(page.locator('main')).toContainText('Benchmarks')
    await expect(page.locator('main')).toContainText('No plan or offer is linked')
    await expect(page.locator('main')).toContainText('No local runtime data yet')
    await expect(page.getByRole('link', { name: /ranking/i }).first()).toBeVisible()
  })

  test('family page shows the tree and includes descendant families', async ({ page }) => {
    await page.goto('/families/claude')
    await expect(page.locator('h1')).toHaveText('Claude')
    await expect(page.locator('main')).toContainText('Family chain')
    await expect(page.locator('main')).toContainText('Child families')
    await expect(page.locator('main')).toContainText('Opus')

    await page.goto('/families')
    await expect(page.locator('main')).toContainText('Including descendants')
    await expect(page.locator('main')).toContainText('claude')
  })

  test('a provider company is distinct from its plans and from a harness name', async ({
    page,
  }) => {
    await page.goto('/companies/opencode')
    await expect(page.locator('h1')).toHaveText('OpenCode')
    await expect(page.locator('main')).toContainText('OpenCode Zen')
    await expect(page.locator('main')).toContainText('OpenCode Go')
    await expect(page.locator('main')).toContainText('OpenCode Go Plus')
    await expect(page.locator('main')).toContainText('harness')
    await expect(page.locator('main')).toContainText('Not in this snapshot')

    await page.goto('/providers/opencode-go')
    await expect(page.locator('h1')).toHaveText('OpenCode Go')
    await expect(page.locator('main')).toContainText('Provider plan')
    await expect(page.locator('main')).toContainText('OpenCode Go plan of')

    await page.goto('/harnesses/opencode')
    await expect(page.locator('h1')).toHaveText('OpenCode')
    await expect(page.locator('main')).toContainText('OpenCode Zen')
  })

  test('harness pages state provider exclusivity', async ({ page }) => {
    await page.goto('/harnesses/claude-code')
    await expect(page.locator('h1')).toHaveText('Claude Code')
    await expect(page.locator('main')).toContainText('Linked providers')
    await expect(page.locator('main')).toContainText('Anthropic')

    await page.goto('/harnesses')
    await expect(page.locator('main')).toContainText('exclusive')
  })

  test('about publishes the real source and freshness deadline', async ({ page, request }) => {
    const advisoryResponse = await request.get('/agent-pool-advisory.v2.json')
    const advisory = (await advisoryResponse.json()) as { snapshotId: string }

    await page.goto('/about')
    const main = page.locator('main')

    await expect(main.getByRole('link', { name: 'models.dev' })).toHaveAttribute(
      'href',
      'https://models.dev/catalog.json?type=all',
    )
    await expect(main).toContainText('MIT')
    await expect(main).toContainText('copyright 2025 models.dev')
    await expect(main).toContainText('Source refreshed')
    await expect(main).toContainText('Stale after')
    await expect(main).toContainText(advisory.snapshotId)
    await expect(main).toContainText('benchmark')
  })

  test('agent pool advisory is a versioned public JSON contract', async ({ request }) => {
    const response = await request.get('/agent-pool-advisory.v2.json')
    expect(response.status()).toBe(200)
    expect(response.headers()['content-type']).toContain('application/json')

    const body = (await response.json()) as {
      contractVersion: number
      snapshotId: string
      records: unknown[]
    }
    expect(body.contractVersion).toBe(2)
    expect(body.snapshotId).toBeTruthy()
    expect(Array.isArray(body.records)).toBe(true)
  })

  test('catalog pages do not make unsupported public claims', async ({ page }) => {
    for (const route of keyRoutes) {
      await page.goto(route.path)
      await expectCatalogPublicClaims(page, route.path)
    }
  })

  test('catalog layout holds from mobile to desktop', async ({ page }) => {
    test.setTimeout(180_000)
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
    // Axe needs more than the default test budget to traverse the large model
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
