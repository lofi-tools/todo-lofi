import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'

const routes = [
  ['Overview', '/'],
  ['Foundations', '/foundations'],
  ['Layout', '/layout'],
  ['Primitives', '/primitives'],
  ['Marketing', '/marketing'],
  ['Product UI', '/product-ui'],
  ['Home replica', '/replicas/home'],
  ['Pricing replica', '/replicas/pricing'],
  ['Features replica', '/replicas/features'],
  ['Customers replica', '/replicas/customers'],
  ['Changelog replica', '/replicas/changelog'],
  ['Docs replica', '/replicas/docs'],
] as const

test.describe('showcase routes', () => {
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
  const background = await page.evaluate(
    () => getComputedStyle(document.body).backgroundColor,
  )
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

test('accessibility: WCAG 2.1 AA on the gallery pages', async ({ page }) => {
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
      `${path}: ${serious.map((violation) => violation.id).join(', ')}`,
    ).toEqual([])
  }
})
