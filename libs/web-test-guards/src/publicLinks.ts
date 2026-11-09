import { expect, type Page } from '@playwright/test'

interface LegacyPublicLinkOptions {
  /** The docs site builds `/downloads`; other sites still treat it as legacy. */
  allowDownloads?: boolean
}

export async function expectNoLegacyPublicLinks(
  page: Page,
  path: string,
  options: LegacyPublicLinkOptions = {},
): Promise<void> {
  const selector = options.allowDownloads
    ? 'a[href^="/roadmap"], a[href^="/docs-web"]'
    : 'a[href^="/downloads"], a[href^="/roadmap"], a[href^="/docs-web"]'
  const routes = options.allowDownloads ? 'roadmap or docs-web' : 'download, roadmap or docs-web'
  await expect(page.locator(selector), `${path} has a live legacy ${routes} link`).toHaveCount(0)
}
