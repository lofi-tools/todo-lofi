import { existsSync } from 'node:fs'
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { DuckDBInstance } from '@duckdb/node-api'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { MODELS_DEV_SOURCE } from '../src/models-dev'
import {
  createSnapshot,
  findLatestSnapshot,
  recordSnapshotFailure,
  validateSnapshotFile,
} from '../src/snapshot'
import type { ModelsDevCatalog } from '../src/types'
import {
  FIXED_CONTENT_SHA256,
  FIXED_NOW,
  FIXED_VERIFIED_AT,
  fixtureCatalog,
} from './fixtures'

let outputDirectory: string

beforeEach(async () => {
  outputDirectory = await mkdtemp(path.join(tmpdir(), 'providers-catalog-'))
})

afterEach(async () => {
  await rm(outputDirectory, { recursive: true, force: true })
})

describe('catalog snapshots', () => {
  it('creates a validated snapshot with provenance and a checksum', async () => {
    const report = await createSnapshot({
      catalog: fixtureCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
    })

    expect(report.snapshotId).toBe('20261109T120000Z-aaaaaaaa')
    expect(report.counts).toEqual({
      providers: 1,
      models: 1,
      agents: 0,
      plans: 0,
      freeOffers: 0,
      localRuntimes: 0,
    })
    expect(existsSync(report.snapshotPath)).toBe(true)
    expect(existsSync(report.checksumPath)).toBe(true)
    expect(report.checksum).toMatch(/^[0-9a-f]{64}$/)

    const validation = await validateSnapshotFile(report.snapshotPath)
    expect(validation.ok).toBe(true)
    expect(validation.errors).toEqual([])
    expect(validation.counts.freeOffers).toBe(0)
    expect(validation.knownGaps).toEqual(fixtureCatalog.knownGaps)
  })

  it('detects duplicate recurring offers', async () => {
    const report = await createSnapshot({
      catalog: fixtureCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
    })

    const instance = await DuckDBInstance.create(report.snapshotPath)
    const connection = await instance.connect()
    try {
      await connection.run(
        `INSERT INTO plans (
           plan_id, provider_id, name, usage_model, billing_period, price, currency,
           included_allowance, allowance_unit, source_id, source_url, source_record,
           verified_at, last_attempt_at, confidence, scrape_status, last_error
         )
         SELECT 'plan-1', provider_id, 'Plan', 'free', NULL, NULL, NULL, NULL, NULL,
                source_id, source_url, 'plan-1', verified_at, last_attempt_at,
                confidence, scrape_status, NULL
         FROM providers LIMIT 1`,
      )
      await connection.run(
        `INSERT INTO free_offers (
           offer_id, provider_id, plan_id, offer_kind, allowance, allowance_unit,
           reset_period, eligibility, conditions, source_id, source_url, source_record,
           verified_at, last_attempt_at, confidence, scrape_status, last_error
         )
         SELECT 'offer-1', provider_id, 'plan-1', 'recurring', 100, 'requests', 'day',
                NULL, NULL, source_id, source_url, 'offer-1', verified_at,
                last_attempt_at, confidence, scrape_status, NULL
         FROM providers LIMIT 1`,
      )
      await connection.run(
        `INSERT INTO free_offers (
           offer_id, provider_id, plan_id, offer_kind, allowance, allowance_unit,
           reset_period, eligibility, conditions, source_id, source_url, source_record,
           verified_at, last_attempt_at, confidence, scrape_status, last_error
         )
         SELECT 'offer-2', provider_id, 'plan-1', 'recurring', 100, 'requests', 'day',
                NULL, NULL, source_id, source_url, 'offer-2', verified_at,
                last_attempt_at, confidence, scrape_status, NULL
         FROM providers LIMIT 1`,
      )
    } finally {
      connection.closeSync()
      instance.closeSync()
    }

    const validation = await validateSnapshotFile(report.snapshotPath)
    expect(validation.ok).toBe(false)
    expect(validation.errors.some((error) => error.includes('duplicate allowance claims'))).toBe(
      true,
    )
  })

  it('warns after seven days without deleting the snapshot', async () => {
    const report = await createSnapshot({
      catalog: fixtureCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
    })

    const later = new Date(FIXED_NOW.getTime() + 8 * 24 * 60 * 60 * 1000)
    const validation = await validateSnapshotFile(report.snapshotPath, later)

    expect(validation.ok).toBe(true)
    expect(validation.stale).toBe(true)
    expect(validation.warnings).toHaveLength(1)
    expect(validation.warnings[0]).toContain('8.0 days old')
  })

  it('does not replace a previous snapshot when the new one fails', async () => {
    const first = await createSnapshot({
      catalog: fixtureCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
    })

    const duplicateModelCatalog: ModelsDevCatalog = {
      ...fixtureCatalog,
      models: [fixtureCatalog.models[0], fixtureCatalog.models[0]],
    }

    await expect(
      createSnapshot({
        catalog: duplicateModelCatalog,
        source: MODELS_DEV_SOURCE,
        fetchedAt: FIXED_VERIFIED_AT,
        contentSha256: 'b'.repeat(64),
        httpStatus: 200,
        etag: null,
        outputDirectory,
        now: new Date(FIXED_NOW.getTime() + 1000),
      }),
    ).rejects.toThrow()

    expect(existsSync(first.snapshotPath)).toBe(true)
    const snapshots = (await readdir(outputDirectory)).filter((name) =>
      name.endsWith('.duckdb'),
    )
    expect(snapshots).toHaveLength(1)
  })

  it('finds the newest snapshot and records failures outside the snapshot', async () => {
    const report = await createSnapshot({
      catalog: fixtureCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
    })

    expect(await findLatestSnapshot(outputDirectory)).toBe(report.snapshotPath)

    const failurePath = await recordSnapshotFailure({
      outputDirectory,
      sourceUrl: MODELS_DEV_SOURCE.sourceUrl,
      error: new Error('source unavailable'),
      occurredAt: FIXED_NOW,
    })
    const failure = JSON.parse(await readFile(failurePath, 'utf8')) as {
      error: string
      sourceUrl: string
    }
    expect(failure).toMatchObject({
      error: 'source unavailable',
      sourceUrl: MODELS_DEV_SOURCE.sourceUrl,
    })
  })
})
