import { expect, type Page } from '@playwright/test'

export async function expectNoLegacyPublicLinks(page: Page, path: string): Promise<void> {
  await expect(
    page.locator('a[href^="/downloads"], a[href^="/roadmap"], a[href^="/docs-web"]'),
    `${path} has a live legacy download, roadmap or docs-web link`,
  ).toHaveCount(0)
}
