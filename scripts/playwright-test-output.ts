import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

export function playwrightOutput(suite: string, cacheDirectory = 'test-output'): string {
  const testOutput = new URL(`../.cache/${cacheDirectory}/`, import.meta.url)
  process.env.PLAYWRIGHT_LAST_RUN_OUTPUT_FILE ??= fileURLToPath(
    new URL(`history/browser/${suite}.json`, testOutput),
  )
  return fileURLToPath(new URL(`runs/browser/${suite}.${randomUUID()}/artifacts`, testOutput))
}
