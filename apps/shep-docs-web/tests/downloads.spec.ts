import { expect, test } from '@playwright/test'
import {
  archFrom,
  assetsFor,
  bundleAssets,
  metadataAssets,
  platformFrom,
  shepherdReleaseFrom,
} from '../src/lib/releases'

const REPO_URL = 'https://github.com/lofi-tools/todo-lofi'

const taskstreamRelease = {
  tag_name: 'v9.9.9',
  html_url: `${REPO_URL}/releases/tag/v9.9.9`,
  published_at: '2026-11-08T10:00:00Z',
  assets: [
    {
      name: 'taskstream-macos.zip',
      browser_download_url: 'https://example.test/taskstream-macos.zip',
      size: 1,
    },
  ],
}

const shepherdRelease = {
  tag_name: 'shepherd-v0.1.0',
  html_url: `${REPO_URL}/releases/tag/shepherd-v0.1.0`,
  published_at: '2026-11-09T20:00:00Z',
  assets: [
    ['shepherd-0.1.0-macos-arm64.zip', 'zip'],
    ['shepherd-0.1.0-macos-x86_64.zip', 'zip'],
    ['shepherd-0.1.0-linux-x86_64.deb', 'deb'],
    ['shepherd-0.1.0-linux-arm64.deb', 'deb'],
    ['shepherd-0.1.0-windows-x86_64-setup.exe', 'exe'],
    ['release-metadata-macos-arm64.json', 'json'],
    ['release-metadata-macos-arm64.sha256', 'sha256'],
  ].map(([name]) => ({
    name,
    browser_download_url: `https://example.test/${name}`,
    size: 10,
  })),
}

test.describe('release resolution', () => {
  test('accepts only a Shepherd release and ignores other products', () => {
    const release = shepherdReleaseFrom([taskstreamRelease, shepherdRelease])
    expect(release?.tag).toBe('shepherd-v0.1.0')
    expect(release?.version).toBe('0.1.0')
    expect(shepherdReleaseFrom([taskstreamRelease])).toBeNull()
    expect(shepherdReleaseFrom({})).toBeNull()
    expect(shepherdReleaseFrom([])).toBeNull()
  })

  test('never treats an empty Shepherd release as a download', () => {
    const empty = { ...shepherdRelease, assets: [] }
    expect(shepherdReleaseFrom([empty])).toBeNull()
  })

  test('splits bundle assets from checksum metadata', () => {
    const release = shepherdReleaseFrom([shepherdRelease])
    expect(release && bundleAssets(release).map((asset) => asset.name)).toEqual([
      'shepherd-0.1.0-macos-arm64.zip',
      'shepherd-0.1.0-macos-x86_64.zip',
      'shepherd-0.1.0-linux-x86_64.deb',
      'shepherd-0.1.0-linux-arm64.deb',
      'shepherd-0.1.0-windows-x86_64-setup.exe',
    ])
    expect(release && metadataAssets(release).map((asset) => asset.name)).toEqual([
      'release-metadata-macos-arm64.json',
      'release-metadata-macos-arm64.sha256',
    ])
  })

  test('selects the bundle for the visitor platform and architecture', () => {
    const release = shepherdReleaseFrom([shepherdRelease])
    if (!release) throw new Error('fixture release must resolve')

    expect(assetsFor(release, 'macos', 'arm64').map((asset) => asset.name)).toEqual([
      'shepherd-0.1.0-macos-arm64.zip',
    ])
    expect(assetsFor(release, 'macos', 'x86_64').map((asset) => asset.name)).toEqual([
      'shepherd-0.1.0-macos-x86_64.zip',
    ])
    expect(assetsFor(release, 'macos', undefined)).toHaveLength(2)
    expect(assetsFor(release, 'linux', 'arm64').map((asset) => asset.name)).toEqual([
      'shepherd-0.1.0-linux-arm64.deb',
    ])
    expect(assetsFor(release, 'windows', undefined).map((asset) => asset.name)).toEqual([
      'shepherd-0.1.0-windows-x86_64-setup.exe',
    ])
    expect(assetsFor(release, 'unknown', undefined)).toHaveLength(5)
  })

  test('reads the platform and architecture hints it can obtain', () => {
    expect(platformFrom('macOS', '')).toBe('macos')
    expect(platformFrom('Win32', '')).toBe('windows')
    expect(platformFrom('Linux x86_64', '')).toBe('linux')
    expect(platformFrom(undefined, 'Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0)')).toBe('macos')
    expect(platformFrom(undefined, 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)')).toBe('windows')
    expect(platformFrom(undefined, 'Mozilla/5.0 (X11; Linux x86_64)')).toBe('linux')
    expect(platformFrom(undefined, 'Mozilla/5.0 (Linux; Android 14)')).toBe('unknown')
    expect(platformFrom(undefined, 'something else')).toBe('unknown')
    expect(archFrom('arm', '')).toBe('arm64')
    expect(archFrom('x86', '')).toBe('x86_64')
    expect(archFrom(undefined, 'MacIntel')).toBe('x86_64')
    expect(archFrom(undefined, 'unknown')).toBeUndefined()
  })
})

test.describe('downloads page', () => {
  test('lists the assets of the newest Shepherd release', async ({ page }) => {
    const errors: string[] = []
    page.on('console', (message) => {
      if (message.type() === 'error') errors.push(message.text())
    })
    page.on('pageerror', (error) => errors.push(error.message))

    await page.route(/\/repos\/lofi-tools\/todo-lofi\/releases/, (route) =>
      route.fulfill({ json: [taskstreamRelease, shepherdRelease] }),
    )

    const response = await page.goto('/downloads')
    expect(response?.ok()).toBeTruthy()
    await expect(page.locator('h1').first()).toHaveText('Download Shepherd')
    await expect(page.locator('[data-release-version]')).toHaveText('0.1.0')
    await expect(page.locator('[data-release-assets] a')).toHaveCount(5)
    await expect(page.getByRole('link', { name: 'shepherd-0.1.0-macos-arm64.zip' })).toHaveAttribute(
      'href',
      'https://example.test/shepherd-0.1.0-macos-arm64.zip',
    )
    await expect(page.locator('[data-release-checksums] a')).toHaveCount(2)
    await expect(page.locator('[data-release-empty]')).toBeHidden()
    await expect(page.locator('[data-release-error]')).toBeHidden()
    expect(errors).toEqual([])
  })

  test('keeps the honest fallback when no Shepherd release exists', async ({ page }) => {
    await page.route(/\/repos\/lofi-tools\/todo-lofi\/releases/, (route) =>
      route.fulfill({ json: [taskstreamRelease] }),
    )

    await page.goto('/downloads')
    await expect(page.locator('[data-release-empty]')).toBeVisible()
    await expect(page.locator('[data-release-content]')).toBeHidden()
    await expect(page.getByRole('link', { name: 'GitHub releases' })).toHaveAttribute(
      'href',
      `${REPO_URL}/releases`,
    )
  })

  test('falls back to the releases page when GitHub fails', async ({ page }) => {
    await page.route(/\/repos\/lofi-tools\/todo-lofi\/releases/, (route) =>
      route.fulfill({ status: 503, body: 'unavailable' }),
    )

    await page.goto('/downloads')
    await expect(page.locator('[data-release-error]')).toBeVisible()
    await expect(page.locator('[data-release-empty]')).toBeHidden()
    await expect(page.locator('[data-release-content]')).toBeHidden()
  })

  test('the home page shows the source version and a download action', async ({ page }) => {
    await page.goto('/')
    await expect(page.locator('main')).toContainText('Version 0.1.0')
    await expect(page.getByRole('link', { name: 'Download', exact: true })).toHaveAttribute(
      'href',
      '/downloads',
    )
  })
})
