import { defineConfig, devices } from '@playwright/test'

const shardPort = process.env.TEST_SHARD_PORT ? Number(process.env.TEST_SHARD_PORT) : undefined

export default defineConfig({
  testDir: './tests',
  // Shard individual tests while keeping the shared server and window serial.
  fullyParallel: true,
  workers: 1,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://localhost:${shardPort ?? 1421}`,
    colorScheme: 'dark',
    trace: 'on-first-retry',
  },
  // Uses the locally installed Chrome, like the other suites here, so no
  // Playwright browser download is needed.
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chrome' } }],
  webServer: {
    // A port of its own: the Tauri dev server already owns 1420.
    command: `pnpm exec vite --port ${shardPort ?? 1421} --strictPort`,
    url: `http://localhost:${shardPort ?? 1421}`,
    reuseExistingServer: !shardPort && !process.env.CI,
    timeout: 120_000,
  },
})
