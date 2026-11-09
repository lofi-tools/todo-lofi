import type {
  CatalogFreeOffer,
  CatalogLocalRuntime,
  CatalogPlan,
  ModelsDevCatalog,
} from '../src/types'

export const FIXED_NOW = new Date('2026-11-09T12:00:00.000Z')
export const FIXED_FETCHED_AT = '2026-11-09T12:00:00.000Z'
export const FIXED_CONTENT_SHA256 = 'a'.repeat(64)

export const fixtureCatalog: ModelsDevCatalog = {
  providers: [
    {
      providerId: 'acme',
      name: 'Acme',
      npm: '@ai-sdk/openai-compatible',
      apiUrl: 'https://api.acme.example/v1',
      docUrl: 'https://acme.example/docs',
      envKeys: ['ACME_API_KEY'],
      logoPath: null,
      retained: false,
      sourceId: 'models-dev',
      sourceUrl: 'https://models.dev/catalog.json?type=all',
      sourceRecord: 'acme',
    },
  ],
  models: [
    {
      providerId: 'acme',
      modelId: 'free-model',
      modelType: null,
      canonicalModelId: 'acme/free-model',
      name: 'Free Model',
      family: 'acme',
      familyId: 'acme',
      description: 'A zero-priced model that is not a free offer.',
      releaseDate: '2026-01-01',
      lastUpdated: '2026-01-01',
      knowledgeDate: '2025-12-01',
      openWeights: true,
      attachment: true,
      reasoning: true,
      toolCall: true,
      structuredOutput: true,
      temperature: true,
      interleaved: null,
      contextLimit: 128000,
      inputLimit: 120000,
      outputLimit: 8192,
      costInput: 0,
      costOutput: 0,
      costReasoning: null,
      costCacheRead: null,
      costCacheWrite: null,
      costInputAudio: null,
      costOutputAudio: null,
      modalitiesInput: ['text'],
      modalitiesOutput: ['text'],
      status: null,
      retained: false,
      sourceId: 'models-dev',
      sourceUrl: 'https://models.dev/catalog.json?type=all',
      sourceRecord: 'acme/free-model',
    },
  ],
  knownGaps: ['No plans or recurring free offers are present in this fixture.'],
}

export function fixturePlan(overrides: Partial<CatalogPlan> = {}): CatalogPlan {
  return {
    planId: 'plan-1',
    providerId: 'acme',
    name: 'Acme Free',
    usageModel: 'free',
    billingPeriod: 'month',
    price: 0,
    currency: 'USD',
    includedAllowance: 100,
    allowanceUnit: 'requests',
    sourceId: 'models-dev',
    sourceUrl: 'https://models.dev/catalog.json?type=all',
    sourceRecord: 'acme/plan-1',
    ...overrides,
  }
}

export function fixtureLocalRuntime(
  overrides: Partial<CatalogLocalRuntime> = {},
): CatalogLocalRuntime {
  return {
    providerId: 'acme',
    modelId: 'free-model',
    runtime: 'llama.cpp',
    quantization: 'Q4_K_M',
    approximateHardware: '16 GB RAM',
    setupNotes: null,
    sourceId: 'models-dev',
    sourceUrl: 'https://models.dev/catalog.json?type=all',
    sourceRecord: 'acme/free-model/llama.cpp',
    ...overrides,
  }
}

export function fixtureFreeOffer(
  overrides: Partial<CatalogFreeOffer> = {},
): CatalogFreeOffer {
  return {
    offerId: 'offer-1',
    providerId: 'acme',
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
    ...overrides,
  }
}
