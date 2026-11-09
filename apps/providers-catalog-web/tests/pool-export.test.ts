import { describe, expect, it } from 'vitest'
import type { CatalogData, CatalogModel } from '../src/lib/catalog'
import { buildShepherdPoolCatalog } from '../src/lib/pool-export'

const GENERATED_AT = '2026-11-09T12:00:00.000Z'

function model(providerId: string, modelId: string): CatalogModel {
  return {
    providerId,
    providerName: providerId,
    modelId,
    modelType: 'language',
    canonicalModelId: `${providerId}/${modelId}`,
    canonicalModelName: modelId,
    familyId: providerId,
    name: `${providerId} ${modelId}`,
    family: null,
    description: null,
    releaseDate: null,
    lastUpdated: null,
    knowledgeDate: null,
    openWeights: false,
    attachment: false,
    reasoning: false,
    toolCall: true,
    structuredOutput: false,
    temperature: true,
    interleaved: null,
    contextLimit: 1000,
    inputLimit: null,
    outputLimit: null,
    costInput: null,
    costOutput: null,
    costReasoning: null,
    costCacheRead: null,
    costCacheWrite: null,
    costInputAudio: null,
    costOutputAudio: null,
    modalitiesInput: null,
    modalitiesOutput: null,
    status: null,
    retained: false,
    sourceId: 'fixture',
    sourceUrl: 'https://fixture.example/catalog.json',
    sourceRecord: `${providerId}/${modelId}`,
  }
}

function fixtureCatalog(): CatalogData {
  return {
    snapshotPath: '/fixture/catalog.duckdb',
    snapshot: {
      snapshotId: 'fixture-snapshot',
      schemaVersion: 3,
      generatedAt: GENERATED_AT,
      sourceFetchedAt: GENERATED_AT,
      freshnessDeadline: '2026-11-16T12:00:00.000Z',
      stale: false,
      ageDays: 0,
      license: 'MIT',
      attribution: 'Fixture data',
      sourceUrls: { fixture: 'https://fixture.example/catalog.json' },
      sources: [],
      knownGaps: [],
    },
    providers: [
      ['anthropic', 'Anthropic'],
      ['openai', 'OpenAI'],
      ['opencode', 'OpenCode'],
      ['google', 'Google'],
    ].map(([providerId, name]) => ({
      providerId,
      name,
      npm: null,
      apiUrl: null,
      docUrl: null,
      envKeys: [],
      retained: false,
      sourceId: 'fixture',
      sourceUrl: 'https://fixture.example/catalog.json',
      sourceRecord: providerId,
      modelCount: 1,
      openWeightCount: 0,
      planCount: 0,
      offerCount: 0,
      localRuntimeCount: 0,
    })),
    models: [
      model('anthropic', 'claude-opus'),
      model('anthropic', 'claude-sonnet'),
      model('openai', 'gpt-codex'),
      model('opencode', 'zen-model'),
      model('google', 'gemini-pro'),
    ],
    canonicalModels: [],
    families: [],
    companies: [],
    companyPlans: [],
    harnesses: [
      { harnessId: 'claude-code', name: 'Claude Code', vendor: 'Anthropic', homepageUrl: null },
      { harnessId: 'codex', name: 'Codex', vendor: 'OpenAI', homepageUrl: null },
      { harnessId: 'opencode', name: 'OpenCode', vendor: 'OpenCode', homepageUrl: null },
      { harnessId: 'gemini-cli', name: 'Gemini CLI', vendor: 'Google', homepageUrl: null },
    ],
    providerHarnesses: [
      { providerId: 'anthropic', harnessId: 'claude-code', exclusive: true, sourceUrl: '' },
      { providerId: 'openai', harnessId: 'codex', exclusive: true, sourceUrl: '' },
      { providerId: 'opencode', harnessId: 'opencode', exclusive: false, sourceUrl: '' },
      { providerId: 'google', harnessId: 'gemini-cli', exclusive: true, sourceUrl: '' },
    ],
    benchmarks: [],
    benchmarkScores: [],
    plans: [
      {
        planId: 'anthropic-free',
        providerId: 'anthropic',
        providerName: 'Anthropic',
        name: 'Claude Free',
        usageModel: 'free',
        billingPeriod: 'month',
        price: 0,
        currency: 'USD',
        includedAllowance: 100,
        allowanceUnit: 'requests',
        sourceId: 'fixture',
        sourceUrl: 'https://fixture.example/catalog.json',
        sourceRecord: 'anthropic-free',
      },
    ],
    offers: [
      {
        offerId: 'anthropic-free-offer',
        providerId: 'anthropic',
        providerName: 'Anthropic',
        planId: 'anthropic-free',
        offerKind: 'recurring',
        allowance: 100,
        allowanceUnit: 'requests',
        resetPeriod: 'day',
        eligibility: null,
        conditions: null,
        sourceId: 'fixture',
        sourceUrl: 'https://fixture.example/catalog.json',
        sourceRecord: 'anthropic-free-offer',
      },
      {
        offerId: 'opencode-one',
        providerId: 'opencode',
        providerName: 'OpenCode',
        planId: null,
        offerKind: 'recurring',
        allowance: 10,
        allowanceUnit: 'requests',
        resetPeriod: 'day',
        eligibility: null,
        conditions: null,
        sourceId: 'fixture',
        sourceUrl: 'https://fixture.example/catalog.json',
        sourceRecord: 'opencode-one',
      },
      {
        offerId: 'opencode-two',
        providerId: 'opencode',
        providerName: 'OpenCode',
        planId: null,
        offerKind: 'recurring',
        allowance: 20,
        allowanceUnit: 'requests',
        resetPeriod: 'day',
        eligibility: null,
        conditions: null,
        sourceId: 'fixture',
        sourceUrl: 'https://fixture.example/catalog.json',
        sourceRecord: 'opencode-two',
      },
    ],
    localRuntimes: [],
  }
}

describe('Shepherd pool export', () => {
  it('covers only the harnesses Shepherd implements', () => {
    const payload = buildShepherdPoolCatalog(fixtureCatalog())
    expect(payload.harnesses.map((harness) => harness.harnessId)).toEqual([
      'claude-code',
      'codex',
      'opencode',
    ])
    expect(payload.harnesses.map((harness) => harness.agent)).toEqual([
      'claude',
      'codex',
      'opencode',
    ])
  })

  it('offers provider and model choices for a harness', () => {
    const payload = buildShepherdPoolCatalog(fixtureCatalog())
    const codex = payload.harnesses.find((harness) => harness.harnessId === 'codex')
    expect(codex?.providers).toEqual([
      {
        providerId: 'openai',
        providerName: 'OpenAI',
        models: [{ modelId: 'gpt-codex', name: 'openai gpt-codex', contextLimit: 1000 }],
      },
    ])
  })

  it('expands a single recurring offer across the provider models', () => {
    const payload = buildShepherdPoolCatalog(fixtureCatalog())
    expect(payload.records).toHaveLength(2)
    expect(payload.records.map((record) => record.modelId)).toEqual([
      'claude-opus',
      'claude-sonnet',
    ])
    expect(payload.records.every((record) => record.capacity === 'known')).toBe(true)
    expect(payload.records.every((record) => record.allowance === 100)).toBe(true)
  })

  it('keeps choices but refuses an ambiguous or missing offer', () => {
    const payload = buildShepherdPoolCatalog(fixtureCatalog())
    const opencode = payload.harnesses.find((harness) => harness.harnessId === 'opencode')
    expect(opencode?.providers.map((provider) => provider.providerId)).toEqual(['opencode'])
    expect(payload.records.some((record) => record.providerId === 'opencode')).toBe(false)
    expect(payload.records.some((record) => record.providerId === 'openai')).toBe(false)
  })

  it('marks every record unknown when the snapshot is stale', () => {
    const catalog = fixtureCatalog()
    catalog.snapshot.stale = true
    const payload = buildShepherdPoolCatalog(catalog)
    expect(payload.stale).toBe(true)
    expect(payload.records.every((record) => record.capacity === 'unknown')).toBe(true)
  })
})
