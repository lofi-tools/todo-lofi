import { defineConfig, devices } from '@playwright/test'

const shardPort = process.env.TEST_SHARD_PORT ? Number(process.env.TEST_SHARD_PORT) : undefined

export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://localhost:${shardPort ?? 4321}`,
    trace: 'on-first-retry',
  },
  // Use the locally installed Chrome so the suite runs without downloading
  // Playwright's own browser bundle (`pnpm exec playwright install`).
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chrome' } }],
  webServer: {
    // Build + preview so the tests exercise the real static output.
    command: shardPort
      ? `pnpm run build && pnpm exec astro preview --ignore-lock --port ${shardPort}`
      : 'pnpm run build && pnpm run preview',
    url: `http://localhost:${shardPort ?? 4321}`,
    reuseExistingServer: !shardPort && !process.env.CI,
    timeout: 180_000,
  },
})
