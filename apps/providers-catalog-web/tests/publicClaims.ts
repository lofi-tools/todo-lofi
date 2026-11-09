import { expect, type Page } from '@playwright/test'
import { expectNoLegacyPublicLinks } from 'web-test-guards/publicLinks'

const forbiddenClaims = [
  /\bavailable now\b/i,
  /\balways free\b/i,
  /\bfree forever\b/i,
  /\bguaranteed free\b/i,
  /\bguaranteed quota\b/i,
  /\bguaranteed available\b/i,
]

const internalNames = /\b(?:taskstream|pandacode|herdr(?:-core|-app)?|freebuff)\b/i

export async function expectCatalogPublicClaims(page: Page, path: string): Promise<void> {
  await expectNoLegacyPublicLinks(page, path)

  const main = page.locator('main')
  const mainText = await main.innerText()
  const bodyText = await page.locator('body').innerText()

  expect(bodyText, `${path} names an internal project`).not.toMatch(internalNames)

  for (const claim of forbiddenClaims) {
    expect(mainText, `${path} makes an unverified availability claim`).not.toMatch(claim)
  }

  // The catalog may explain the free-offer policy, but no catalog entry may
  // render a bare "Free" marker while no offer has provenance.
  await expect(
    main.locator('.pill').filter({ hasText: /^free$/i }),
    `${path} renders an unverified Free pill`,
  ).toHaveCount(0)

  expect(
    mainText,
    `${path} must state the plan/offer gap rather than imply offers exist`,
  ).toMatch(/no verified|not published|not yet|does not publish|empty/i)
}
