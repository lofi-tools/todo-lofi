import { defineConfig, devices } from '@playwright/test'

const shardPort = process.env.TEST_SHARD_PORT ? Number(process.env.TEST_SHARD_PORT) : undefined

export default defineConfig({
  testDir: './tests',
  testMatch: '**/*.spec.ts',
  fullyParallel: true,
  workers: 2,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://localhost:${shardPort ?? 4326}`,
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chrome' } }],
  webServer: {
    command: shardPort
      ? `pnpm run build && pnpm exec astro preview --ignore-lock --port ${shardPort}`
      : 'pnpm run e2e:cleanup && pnpm run build && pnpm run e2e:server',
    url: `http://localhost:${shardPort ?? 4326}`,
    reuseExistingServer: false,
    timeout: 240_000,
  },
})
