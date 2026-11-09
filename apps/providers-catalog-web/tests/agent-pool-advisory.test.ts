import { describe, expect, it } from 'vitest'
import {
  applyLocalAllowance,
  buildAgentPoolAdvisory,
  type AgentPoolAdvisoryRecord,
} from '../src/lib/agent-pool-advisory'
import type { CatalogData } from '../src/lib/catalog'

const VERIFIED_AT = '2026-11-09T12:00:00.000Z'
const OBSERVED_AT = '2026-11-09T13:00:00.000Z'

function fixtureCatalog(): CatalogData {
  return {
    snapshotPath: '/fixture/catalog.duckdb',
    snapshot: {
      snapshotId: 'fixture-snapshot',
      schemaVersion: 1,
      generatedAt: VERIFIED_AT,
      sourceFetchedAt: VERIFIED_AT,
      freshnessDeadline: '2026-11-16T12:00:00.000Z',
      stale: false,
      ageDays: 0,
      license: 'MIT',
      attribution: 'Fixture data',
      sourceUrls: { fixture: 'https://fixture.example/catalog.json' },
      sources: [
        {
          sourceId: 'fixture',
          name: 'Fixture',
          sourceUrl: 'https://fixture.example/catalog.json',
          license: 'MIT',
          attribution: 'Fixture data',
          fetchedAt: VERIFIED_AT,
          httpStatus: 200,
          etag: null,
          contentSha256: 'a'.repeat(64),
          status: 'ok',
          error: null,
        },
      ],
      knownGaps: [],
    },
    providers: [],
    models: [],
    plans: [
      {
        planId: 'plan-1',
        providerId: 'acme',
        providerName: 'Acme',
        name: 'Acme Free',
        usageModel: 'free',
        billingPeriod: 'month',
        price: 0,
        currency: 'USD',
        includedAllowance: 100,
        allowanceUnit: 'requests',
        sourceId: 'fixture',
        sourceUrl: 'https://fixture.example/catalog.json',
        sourceRecord: 'fixture/plan-1',
        verifiedAt: VERIFIED_AT,
        confidence: 'community',
        scrapeStatus: 'ok',
      },
    ],
    offers: [
      {
        offerId: 'offer-1',
        providerId: 'acme',
        providerName: 'Acme',
        planId: 'plan-1',
        offerKind: 'recurring',
        allowance: 100,
        allowanceUnit: 'requests',
        resetPeriod: 'day',
        eligibility: null,
        conditions: 'Renews daily.',
        sourceId: 'fixture',
        sourceUrl: 'https://fixture.example/catalog.json',
        sourceRecord: 'fixture/offer-1',
        verifiedAt: VERIFIED_AT,
        confidence: 'community',
        scrapeStatus: 'ok',
      },
    ],
    agents: [],
    localRuntimes: [],
  }
}

describe('agent pool advisory', () => {
  it('publishes versioned offer records with a known capacity', () => {
    const advisory = buildAgentPoolAdvisory(fixtureCatalog())

    expect(advisory.contractVersion).toBe(1)
    expect(advisory.snapshotId).toBe('fixture-snapshot')
    expect(advisory.records).toHaveLength(1)
    expect(advisory.records[0]).toMatchObject({
      providerId: 'acme',
      modelId: null,
      planId: 'plan-1',
      offerId: 'offer-1',
      allowance: 100,
      allowanceUnit: 'requests',
      resetPeriod: 'day',
      usageModel: 'free',
      capacity: 'known',
      scrapeStatus: 'ok',
      stale: false,
    })
  })

  it('maps stale, missing and unknown-plan records to unknown capacity', () => {
    const staleCatalog = fixtureCatalog()
    staleCatalog.snapshot.stale = true
    expect(buildAgentPoolAdvisory(staleCatalog).records[0].capacity).toBe('unknown')

    const missingCatalog = fixtureCatalog()
    missingCatalog.offers[0].scrapeStatus = 'missing'
    expect(buildAgentPoolAdvisory(missingCatalog).records[0].capacity).toBe('unknown')

    const unknownPlanCatalog = fixtureCatalog()
    unknownPlanCatalog.offers[0].planId = 'missing-plan'
    expect(buildAgentPoolAdvisory(unknownPlanCatalog).records[0]).toMatchObject({
      usageModel: 'unknown',
      capacity: 'unknown',
    })
  })

  it('lets a local allowance observation override a stale catalog allowance', () => {
    const staleCatalog = fixtureCatalog()
    staleCatalog.snapshot.stale = true
    const record = buildAgentPoolAdvisory(staleCatalog).records[0]
    expect(record.capacity).toBe('unknown')

    const overridden = applyLocalAllowance(record, {
      source: 'local_usage',
      allowance: 5,
      allowanceUnit: 'requests',
      resetPeriod: 'minute',
      observedAt: OBSERVED_AT,
    })

    expect(overridden).toMatchObject<AgentPoolAdvisoryRecord>({
      ...record,
      allowance: 5,
      allowanceUnit: 'requests',
      resetPeriod: 'minute',
      capacity: 'known',
      confidence: 'verified',
      verifiedAt: OBSERVED_AT,
    })
  })
})
