import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests',
  // The suite shares one Vite server and drives one window at a time.
  fullyParallel: false,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: 'http://localhost:1421',
    trace: 'on-first-retry',
  },
  // Uses the locally installed Chrome, like the other suites here, so no
  // Playwright browser download is needed.
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chrome' } }],
  webServer: {
    // A port of its own: the Tauri dev server already owns 1420.
    command: 'pnpm exec vite --port 1421 --strictPort',
    url: 'http://localhost:1421',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
})
