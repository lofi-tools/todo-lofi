import { execFileSync } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { createSnapshot } from '../src/snapshot'
import { MODELS_DEV_SOURCE } from '../src/models-dev'
import { planRebuild } from '../scripts/rebuild-from-artifact'
import {
  FIXED_CONTENT_SHA256,
  FIXED_NOW,
  FIXED_FETCHED_AT,
  fixtureCatalog,
} from './fixtures'

describe('rebuild from artifact', () => {
  it('reports recovery steps for a missing snapshot', async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'catalog-rebuild-'))
    try {
      const report = await planRebuild({
        snapshot: path.join(directory, 'expired.duckdb'),
      })

      expect(report.status).toBe('recovery-required')
      expect(report.snapshotPath).toBeNull()
      expect(report.recoverySteps.join(' ')).toContain('Retained GitHub Actions artifacts expire')
      expect(report.recoverySteps.join(' ')).toContain('It never deploys')
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })

  it('selects and validates the newest snapshot in an artifact directory', async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'catalog-rebuild-'))
    try {
      await createSnapshot({
        catalog: fixtureCatalog,
        source: MODELS_DEV_SOURCE,
        fetchedAt: FIXED_FETCHED_AT,
        contentSha256: FIXED_CONTENT_SHA256,
        httpStatus: 200,
        etag: null,
        outputDirectory: directory,
        now: FIXED_NOW,
      })

      const report = await planRebuild({ artifactDirectory: directory })
      expect(report).toMatchObject({
        status: 'ready',
        stale: false,
        errors: [],
        recoverySteps: [],
      })
      expect(report.snapshotPath).toMatch(/providers-catalog-3-.*\.duckdb$/)
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })

  it('exits zero with recovery JSON when the CLI cannot find an artifact', async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'catalog-rebuild-'))
    try {
      const output = execFileSync(
        process.execPath,
        [
          '--import',
          'tsx',
          'scripts/rebuild-from-artifact.ts',
          '--',
          '--snapshot',
          path.join(directory, 'missing.duckdb'),
        ],
        {
          cwd: process.cwd(),
          encoding: 'utf8',
        },
      )
      const report = JSON.parse(output) as { status: string; recoverySteps: string[] }

      expect(report.status).toBe('recovery-required')
      expect(report.recoverySteps.length).toBeGreaterThan(0)
    } finally {
      await rm(directory, { recursive: true, force: true })
    }
  })
})
