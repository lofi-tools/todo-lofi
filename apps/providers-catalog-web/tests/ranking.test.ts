import { describe, expect, it } from 'vitest'
import type { CatalogData, CatalogSnapshot } from '../src/lib/catalog'
import { buildHarnessRanking } from '../src/lib/ranking'
import { FIXED_FETCHED_AT, fixtureCatalog } from './fixtures'

const snapshot: CatalogSnapshot = {
  snapshotId: 'fixture-snapshot',
  schemaVersion: 3,
  generatedAt: FIXED_FETCHED_AT,
  sourceFetchedAt: FIXED_FETCHED_AT,
  freshnessDeadline: '2026-11-16T12:00:00.000Z',
  stale: false,
  ageDays: 0,
  license: 'MIT',
  attribution: 'Fixture data',
  sourceUrls: { 'models-dev': 'https://models.dev/catalog.json?type=all' },
  sources: [],
  knownGaps: [],
}

function catalogWith(overrides: Partial<CatalogData> = {}): CatalogData {
  return {
    snapshotPath: '/fixture/catalog.duckdb',
    snapshot,
    providers: [
      {
        providerId: 'acme',
        name: 'Acme',
        npm: null,
        apiUrl: null,
        docUrl: null,
        envKeys: [],
        retained: false,
        sourceId: 'models-dev',
        sourceUrl: 'https://models.dev/catalog.json?type=all',
        sourceRecord: 'acme',
        modelCount: 1,
        openWeightCount: 1,
        planCount: 0,
        offerCount: 0,
        localRuntimeCount: 0,
      },
    ],
    models: [
      {
        ...fixtureCatalog.models[0],
        providerName: 'Acme',
        canonicalModelName: 'Free Model',
        familyId: 'acme',
      },
    ],
    canonicalModels: [
      {
        canonicalModelId: 'acme/free-model',
        name: 'Free Model',
        lab: 'acme',
        familyId: 'acme',
      },
    ],
    families: [{ familyId: 'acme', name: 'Acme', parentFamilyId: null, vendor: 'acme' }],
    companies: [],
    companyPlans: [],
    harnesses: [
      { harnessId: 'claude-code', name: 'Claude Code', vendor: 'Anthropic', homepageUrl: null },
    ],
    providerHarnesses: [
      {
        providerId: 'acme',
        harnessId: 'claude-code',
        exclusive: true,
        sourceUrl: 'https://example.test/harness',
      },
    ],
    benchmarks: [
      {
        benchmarkId: 'swe-bench-verified',
        name: 'SWE-bench Verified',
        description: null,
        unit: 'percent',
        higherIsBetter: true,
        sourceUrl: 'https://www.swebench.com/',
      },
      {
        benchmarkId: 'terminal-bench',
        name: 'Terminal-Bench',
        description: null,
        unit: 'percent',
        higherIsBetter: true,
        sourceUrl: 'https://www.tbench.ai/',
      },
    ],
    benchmarkScores: [
      {
        canonicalModelId: 'acme/free-model',
        benchmarkId: 'swe-bench-verified',
        score: 80,
        sourceUrl: 'https://www.swebench.com/',
      },
      {
        canonicalModelId: 'acme/free-model',
        benchmarkId: 'terminal-bench',
        score: 90,
        sourceUrl: 'https://www.tbench.ai/',
      },
    ],
    plans: [],
    offers: [
      {
        offerId: 'offer-1',
        providerId: 'acme',
        providerName: 'Acme',
        planId: null,
        offerKind: 'recurring',
        allowance: 100,
        allowanceUnit: 'requests',
        resetPeriod: 'day',
        eligibility: null,
        conditions: null,
        sourceId: 'models-dev',
        sourceUrl: 'https://models.dev/catalog.json?type=all',
        sourceRecord: 'acme/offer-1',
      },
    ],
    localRuntimes: [],
    ...overrides,
  }
}

describe('harness ranking', () => {
  it('multiplies the normalized monthly allowance by the mean model score', () => {
    const ranking = buildHarnessRanking(catalogWith())

    expect(ranking.groups).toHaveLength(1)
    const [group] = ranking.groups
    expect(group.unit).toBe('requests')
    expect(group.entries).toHaveLength(1)
    expect(group.entries[0]).toMatchObject({
      harnessId: 'claude-code',
      averageCodingScore: 85,
      scoredModels: 1,
      score: 100 * 30.4375 * 85,
    })
  })

  it('ranks requests and tokens separately instead of comparing them', () => {
    const catalog = catalogWith({
      offers: [
        {
          ...catalogWith().offers[0],
          allowance: 1_000_000,
          allowanceUnit: 'tokens',
          resetPeriod: 'month',
        },
        {
          ...catalogWith().offers[0],
          offerId: 'offer-2',
          allowance: 10,
          allowanceUnit: 'requests',
          resetPeriod: 'day',
        },
      ],
    })

    const ranking = buildHarnessRanking(catalog)
    expect(ranking.groups.map((group) => group.unit)).toEqual(['requests', 'tokens'])
    expect(ranking.groups[0].entries[0].score).toBe(Math.round(10 * 30.4375 * 85 * 100) / 100)
    expect(ranking.groups[1].entries[0].score).toBe(1_000_000 * 85)
  })

  it('reports no rankable harness when no offer or score is published', () => {
    const ranking = buildHarnessRanking(catalogWith({ offers: [], benchmarkScores: [] }))

    expect(ranking.groups).toEqual([])
    expect(ranking.totalHarnesses).toBe(1)
    expect(ranking.totalRecurringOffers).toBe(0)
    expect(ranking.totalBenchmarkScores).toBe(0)
  })

  it('ignores offers with an allowance unit that cannot be ranked', () => {
    const ranking = buildHarnessRanking(
      catalogWith({
        offers: [{ ...catalogWith().offers[0], allowanceUnit: 'credits' }],
      }),
    )

    expect(ranking.groups).toEqual([])
  })
})
