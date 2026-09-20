import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

const routes = [
  ['Overview', '/'],
  ['Quickstart', '/quickstart'],
  ['Features', '/features'],
  ['Architecture', '/architecture'],
  ['Integrations', '/integrations'],
  ['Contributing', '/contributing'],
  ['License', '/license'],
] as const

test.describe('docs routes', () => {
  for (const [name, path] of routes) {
    test(`${name} renders without console errors`, async ({ page }) => {
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
})

test('the design system stylesheet is applied', async ({ page }) => {
  await page.goto('/')
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
  await page.goto('/')
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')

  await page.getByRole('button', { name: /switch to light theme/i }).first().click()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')

  const stored = await page.evaluate(() => localStorage.getItem('wds-theme'))
  expect(stored).toBe('light')
})

test('the sidebar marks the current page and the pager moves between pages', async ({ page }) => {
  await page.goto('/quickstart')
  await expect(page.locator('nav[aria-label="Documentation"] a[aria-current="page"]')).toHaveText(
    'Quickstart',
  )

  await page.getByRole('link', { name: /next/i }).first().click()
  await expect(page).toHaveURL(/\/features\/?$/)
  await expect(page.locator('h1').first()).toHaveText('Features')
})

test('the layout holds from mobile to desktop without horizontal overflow', async ({ page }) => {
  for (const width of [375, 768, 1440]) {
    await page.setViewportSize({ width, height: 900 })
    for (const [, path] of routes) {
      await page.goto(path)
      const overflow = await page.evaluate(
        () =>
          document.documentElement.scrollWidth - document.documentElement.clientWidth,
      )
      expect(overflow, `${path} at ${width}px wide`).toBeLessThanOrEqual(1)
    }
  }
})

test('the sidebar and table of contents collapse on narrow viewports', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto('/quickstart')
  await expect(page.locator('nav[aria-label="Documentation"]')).toBeVisible()
  await expect(page.locator('nav[aria-label="On this page"]')).toBeVisible()

  await page.setViewportSize({ width: 375, height: 812 })
  await expect(page.locator('nav[aria-label="Documentation"]')).toBeHidden()
  await expect(page.locator('nav[aria-label="Docs sections"]')).toBeVisible()
})

test('accessibility: WCAG 2.1 AA on every docs route', async ({ page }) => {
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
