import { defineConfig, devices } from '@playwright/test'

const shardPort = process.env.TEST_SHARD_PORT ? Number(process.env.TEST_SHARD_PORT) : undefined

export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
  // Two workers keep local Chrome context launches stable while the suite
  // stays parallel.
  workers: 2,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    // Its own port: the showcase previews on 4321 and Shepherd docs on 4324.
    baseURL: `http://localhost:${shardPort ?? 4322}`,
    trace: 'on-first-retry',
  },
  // Use the locally installed Chrome so the suite runs without downloading
  // Playwright's own browser bundle (`pnpm exec playwright install`).
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chrome' } }],
  webServer: {
    command: shardPort
      ? `pnpm run build && pnpm exec astro preview --ignore-lock --port ${shardPort}`
      : 'pnpm run e2e:cleanup && pnpm run build && pnpm run e2e:server',
    url: `http://localhost:${shardPort ?? 4322}`,
    reuseExistingServer: false,
    timeout: 180_000,
  },
})
