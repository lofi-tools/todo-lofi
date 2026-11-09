import { describe, expect, it } from 'vitest'
import type { CatalogData, CatalogModel, CatalogSnapshot } from '../src/lib/catalog'
import { buildComparisonModels } from '../src/lib/compare'
import {
  FIXED_CONTENT_SHA256,
  FIXED_VERIFIED_AT,
  fixtureCatalog,
  fixtureFreeOffer,
  fixtureLocalRuntime,
  fixturePlan,
} from './fixtures'

const SOURCE_URL = 'https://models.dev/catalog.json?type=all'
const ATTRIBUTION = 'models.dev (https://models.dev), copyright 2025 models.dev, MIT License'

const snapshot: CatalogSnapshot = {
  snapshotId: 'fixture-snapshot',
  schemaVersion: 1,
  generatedAt: FIXED_VERIFIED_AT,
  sourceFetchedAt: FIXED_VERIFIED_AT,
  freshnessDeadline: '2026-11-16T12:00:00.000Z',
  stale: false,
  ageDays: 0,
  license: 'MIT',
  attribution: ATTRIBUTION,
  sourceUrls: { 'models-dev': SOURCE_URL },
  sources: [
    {
      sourceId: 'models-dev',
      name: 'models.dev',
      sourceUrl: SOURCE_URL,
      license: 'MIT',
      attribution: ATTRIBUTION,
      fetchedAt: FIXED_VERIFIED_AT,
      httpStatus: 200,
      etag: null,
      contentSha256: FIXED_CONTENT_SHA256,
      status: 'ok',
      error: null,
    },
  ],
  knownGaps: [],
}

const model: CatalogModel = {
  ...fixtureCatalog.models[0],
  providerName: 'Acme',
}

function catalogWith(overrides: Partial<CatalogData> = {}): CatalogData {
  return {
    snapshotPath: '/fixture/catalog.duckdb',
    snapshot,
    providers: [],
    models: [model],
    plans: [{ ...fixturePlan(), providerName: 'Acme' }],
    offers: [{ ...fixtureFreeOffer(), providerName: 'Acme' }],
    agents: [],
    localRuntimes: [fixtureLocalRuntime()],
    ...overrides,
  }
}

describe('comparison data', () => {
  it('carries real plan, offer and local-runtime facts with their source', () => {
    const [entry] = buildComparisonModels(catalogWith())

    expect(entry.sourceName).toBe('models.dev')
    expect(entry.sourceUrl).toBe(SOURCE_URL)
    expect(entry.plans).toEqual([
      {
        name: 'Acme Free',
        usageModel: 'free',
        sourceName: 'models.dev',
        sourceUrl: SOURCE_URL,
      },
    ])
    expect(entry.offers).toEqual([
      {
        allowance: 100,
        allowanceUnit: 'requests',
        resetPeriod: 'day',
        conditions: null,
        sourceName: 'models.dev',
        sourceUrl: SOURCE_URL,
      },
    ])
    expect(entry.localRuntimes).toEqual([
      {
        runtime: 'llama.cpp',
        quantization: 'Q4_K_M',
        approximateHardware: '16 GB RAM',
        sourceName: 'models.dev',
        sourceUrl: SOURCE_URL,
      },
    ])
  })

  it('leaves unknown plan, offer and runtime facts empty instead of inventing values', () => {
    const [entry] = buildComparisonModels(
      catalogWith({ plans: [], offers: [], localRuntimes: [] }),
    )

    expect(entry.plans).toEqual([])
    expect(entry.offers).toEqual([])
    expect(entry.localRuntimes).toEqual([])
  })

  it('falls back to the record URL when the snapshot has no matching source row', () => {
    const [entry] = buildComparisonModels(
      catalogWith({ snapshot: { ...snapshot, sources: [] } }),
    )

    expect(entry.sourceName).toBe(SOURCE_URL)
    expect(entry.sourceUrl).toBe(SOURCE_URL)
  })
})
