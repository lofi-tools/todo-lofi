import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const testOutput = new URL('../.cache/test-output/', import.meta.url)

export function playwrightOutput(suite: string): string {
  process.env.PLAYWRIGHT_LAST_RUN_OUTPUT_FILE ??= fileURLToPath(
    new URL(`history/browser/${suite}.json`, testOutput),
  )
  return fileURLToPath(new URL(`runs/browser/${suite}.${randomUUID()}/artifacts`, testOutput))
}
