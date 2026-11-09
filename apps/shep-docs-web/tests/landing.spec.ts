import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

test.describe('Shepherd landing page', () => {
  test('renders the promise and marks available versus planned work', async ({ page }) => {
    const errors: string[] = []
    page.on('console', (message) => {
      if (message.type() === 'error') errors.push(message.text())
    })
    page.on('pageerror', (error) => errors.push(error.message))

    const response = await page.goto('/')
    expect(response?.ok()).toBeTruthy()

    const main = page.locator('main')
    await expect(page.locator('h1').first()).toHaveText('Several coding agents. One workspace.')
    await expect(main).toContainText(/without babysitting rate limits/i)
    await expect(main).toContainText(/agent pool/i)
    await expect(main).toContainText(/one local workspace/i)
    await expect(main).toContainText(/preview and annotation/i)
    await expect(main).toContainText(/worktree/i)
    await expect(main).toContainText(/approval/i)
    await expect(main).toContainText(/terminal/i)

    await expect(page.locator('#available')).toContainText('Available now')
    await expect(page.locator('#available')).toContainText('Next')
    await expect(page.locator('#next')).toContainText(/not in the current build/i)
    await expect(page.locator('header nav[aria-label="Main"] a[aria-current="page"]')).toHaveText(
      /shepherd/i,
    )

    await expect(page.locator('body')).not.toContainText(/taskstream/i)
    expect(await main.innerText()).not.toMatch(/pandacode|herdr(?:-core|-app)?\b|\bpi\b/i)
    expect(errors).toEqual([])
  })

  test('the hero CTA and header links move between availability sections', async ({ page }) => {
    await page.goto('/')

    await page.getByRole('link', { name: /see what works today/i }).first().click()
    await expect(page).toHaveURL(/#available$/)
    await expect(page.locator('#available')).toBeVisible()

    await page.locator('header nav[aria-label="Main"]').getByRole('link', { name: 'Next' }).click()
    await expect(page).toHaveURL(/#next$/)
    await expect(page.locator('#next')).toBeVisible()
  })

  test('the mobile drawer exposes the same section links', async ({ page }) => {
    await page.setViewportSize({ width: 375, height: 812 })
    await page.goto('/')

    const header = page.locator('header')
    const menu = header.getByRole('button', { name: 'Menu' })
    await expect(menu).toBeVisible()
    await menu.click()
    await expect(menu).toHaveAttribute('aria-expanded', 'true')

    const drawer = page.locator('#site-menu')
    await expect(drawer.getByRole('link', { name: 'Available' })).toBeVisible()
    await drawer.getByRole('link', { name: 'Available' }).click()
    await expect(page).toHaveURL(/#available$/)
  })

  test('the source CTA and in-page anchors resolve', async ({ page }) => {
    await page.goto('/')

    await expect(page.locator('main a[href="#available"]').first()).toBeVisible()
    await expect(page.getByRole('link', { name: /run from source/i })).toHaveAttribute(
      'href',
      /apps\/shepherd\/README\.md$/,
    )
    await expect(page.locator('a[href="/downloads"], a[href="/roadmap"]')).toHaveCount(0)
  })

  test('the design system stylesheet is applied and the theme persists', async ({ page }) => {
    await page.goto('/')

    const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor)
    expect(background).not.toBe('rgba(0, 0, 0, 0)')
    expect(background).not.toBe('')

    const token = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--colors-accent').trim(),
    )
    expect(token.length).toBeGreaterThan(0)

    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    await page.getByRole('button', { name: /switch to light theme/i }).first().click()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
    expect(await page.evaluate(() => localStorage.getItem('wds-theme'))).toBe('light')
  })

  test('the layout holds from mobile to desktop without horizontal overflow', async ({ page }) => {
    for (const width of [375, 768, 1440]) {
      await page.setViewportSize({ width, height: 900 })
      await page.goto('/')
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      )
      expect(overflow, `at ${width}px wide`).toBeLessThanOrEqual(1)
    }
  })

  test('accessibility: WCAG 2.1 AA on the landing page', async ({ page }) => {
    await page.goto('/')
    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
      .analyze()
    const serious = results.violations.filter(
      (violation) => violation.impact === 'serious' || violation.impact === 'critical',
    )
    expect(
      serious,
      serious.map((violation) => `${violation.id} (${violation.nodes.length})`).join(', '),
    ).toEqual([])
  })
})
