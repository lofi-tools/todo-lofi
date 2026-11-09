import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
  // Two workers keep local Chrome context launches stable while the suite
  // stays parallel.
  workers: 2,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    // Its own port: the showcase previews on 4321 and Taskstream docs on 4322.
    baseURL: 'http://localhost:4324',
    trace: 'on-first-retry',
  },
  // Use the locally installed Chrome so the suite runs without downloading
  // Playwright's own browser bundle (`pnpm exec playwright install`).
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chrome' } }],
  webServer: {
    // Stop a stale preview left by an earlier run, then build and serve the
    // real static output on the suite's own port.
    command: 'pnpm run e2e:cleanup && pnpm run build && pnpm run e2e:server',
    url: 'http://localhost:4324',
    reuseExistingServer: false,
    timeout: 180_000,
  },
})
