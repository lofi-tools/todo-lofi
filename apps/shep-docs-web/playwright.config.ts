import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
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
    // Build + preview so the tests exercise the real static output.
    command: 'pnpm run build && pnpm run preview',
    url: 'http://localhost:4324',
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
})
