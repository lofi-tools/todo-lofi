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
  fixtureAgent,
  fixtureCatalog,
  fixtureFreeOffer,
  fixtureLocalRuntime,
  fixturePlan,
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

  it('writes the S2 fixture snapshot with an agent, plan, recurring offer and local runtime', async () => {
    const report = await createSnapshot({
      catalog: { ...fixtureCatalog, knownGaps: [] },
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
      agents: [fixtureAgent()],
      plans: [fixturePlan()],
      freeOffers: [fixtureFreeOffer({ planId: 'plan-1' })],
      localRuntimes: [fixtureLocalRuntime()],
    })

    expect(report.counts).toEqual({
      providers: 1,
      models: 1,
      agents: 1,
      plans: 1,
      freeOffers: 1,
      localRuntimes: 1,
    })
    expect(report.offerMerge).toEqual({ input: 1, written: 1, mergedOfferIds: [] })

    const validation = await validateSnapshotFile(report.snapshotPath)
    expect(validation.ok).toBe(true)
    expect(validation.errors).toEqual([])
    expect(validation.knownGaps).toEqual([])
  })

  it('merges duplicate recurring offers before writing and reports the merged ids', async () => {
    const report = await createSnapshot({
      catalog: fixtureCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: FIXED_VERIFIED_AT,
      contentSha256: FIXED_CONTENT_SHA256,
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: FIXED_NOW,
      freeOffers: [
        fixtureFreeOffer(),
        fixtureFreeOffer({ offerId: 'offer-2', sourceRecord: 'acme/offer-2' }),
      ],
    })

    expect(report.counts.freeOffers).toBe(1)
    expect(report.offerMerge).toEqual({
      input: 2,
      written: 1,
      mergedOfferIds: ['offer-2'],
    })

    const validation = await validateSnapshotFile(report.snapshotPath)
    expect(validation.ok).toBe(true)
  })

  it('rejects one-time and expiring trial wording during validation', async () => {
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
        `INSERT INTO free_offers (
           offer_id, provider_id, plan_id, offer_kind, allowance, allowance_unit,
           reset_period, eligibility, conditions, source_id, source_url, source_record,
           verified_at, last_attempt_at, confidence, scrape_status, last_error
         )
         SELECT 'offer-trial', provider_id, NULL, 'recurring', 100, 'requests', 'day',
                NULL, 'Free for 30 days; limited-time trial.', source_id, source_url,
                'offer-trial', verified_at, last_attempt_at, confidence, scrape_status, NULL
         FROM providers LIMIT 1`,
      )
    } finally {
      connection.closeSync()
      instance.closeSync()
    }

    const validation = await validateSnapshotFile(report.snapshotPath)
    expect(validation.ok).toBe(false)
    expect(
      validation.errors.some(
        (error) => error.includes('policy-excluded claims') && error.includes('offer-trial'),
      ),
    ).toBe(true)
  })

  it('retains facts omitted by a later successful scrape and reports them for review', async () => {
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

    const replacementCatalog: ModelsDevCatalog = {
      providers: [
        {
          ...fixtureCatalog.providers[0],
          providerId: 'beta',
          name: 'Beta',
          sourceRecord: 'beta',
        },
      ],
      models: [
        {
          ...fixtureCatalog.models[0],
          providerId: 'beta',
          modelId: 'new-model',
          canonicalModelId: 'beta/new-model',
          name: 'New Model',
          sourceRecord: 'beta/new-model',
        },
      ],
      knownGaps: fixtureCatalog.knownGaps,
    }

    const second = await createSnapshot({
      catalog: replacementCatalog,
      source: MODELS_DEV_SOURCE,
      fetchedAt: '2026-11-09T12:01:00.000Z',
      contentSha256: 'b'.repeat(64),
      httpStatus: 200,
      etag: null,
      outputDirectory,
      now: new Date(FIXED_NOW.getTime() + 60_000),
      previousSnapshotPath: first.snapshotPath,
    })

    expect(second.counts.providers).toBe(2)
    expect(second.counts.models).toBe(2)
    expect(second.merge).toMatchObject({
      previousSnapshotPath: first.snapshotPath,
      retainedProviders: ['acme'],
      retainedModels: ['acme/free-model'],
    })
    expect(second.merge?.reviewItems).toEqual(['model acme/free-model', 'provider acme'])

    const instance = await DuckDBInstance.create(second.snapshotPath, { access_mode: 'READ_ONLY' })
    const connection = await instance.connect()
    try {
      const reader = await connection.runAndReadAll(
        `SELECT provider_id, scrape_status, last_error,
                epoch_ms(verified_at) AS verified_at_ms,
                epoch_ms(last_attempt_at) AS last_attempt_at_ms
         FROM providers ORDER BY provider_id`,
      )
      const providers = reader.getRowObjects()
      const acme = providers.find((row) => row.provider_id === 'acme')
      const beta = providers.find((row) => row.provider_id === 'beta')

      expect(acme?.scrape_status).toBe('missing')
      expect(acme?.last_error).toContain('latest source response')
      expect(Number(acme?.verified_at_ms)).toBe(new Date(FIXED_VERIFIED_AT).getTime())
      expect(Number(acme?.last_attempt_at_ms)).toBe(
        new Date('2026-11-09T12:01:00.000Z').getTime(),
      )
      expect(beta?.scrape_status).toBe('ok')

      const modelReader = await connection.runAndReadAll(
        `SELECT model_id, scrape_status FROM models ORDER BY model_id`,
      )
      expect(modelReader.getRowObjects()).toEqual([
        expect.objectContaining({ model_id: 'free-model', scrape_status: 'missing' }),
        expect.objectContaining({ model_id: 'new-model', scrape_status: 'ok' }),
      ])
    } finally {
      connection.closeSync()
      instance.closeSync()
    }
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
