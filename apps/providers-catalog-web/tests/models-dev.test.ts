import { describe, expect, it } from 'vitest'
import { familyChain } from '../src/curated'
import { parseModelsDevCatalog } from '../src/models-dev'

const source = {
  providers: {
    acme: {
      id: 'acme',
      name: 'Acme',
      npm: '@ai-sdk/openai-compatible',
      api: 'https://api.acme.example/v1',
      doc: 'https://acme.example/docs',
      env: ['ACME_API_KEY'],
      models: {
        'free-model': {
          id: 'free-model',
          name: 'Free Model',
          description: 'A zero-priced model.',
          family: 'acme',
          attachment: true,
          reasoning: true,
          tool_call: true,
          structured_output: true,
          temperature: true,
          release_date: '2026-01-01',
          last_updated: '2026-01-01',
          knowledge: '2025-12-01',
          open_weights: true,
          limit: { context: 128000, input: 120000, output: 8192 },
          cost: { input: 0, output: 0, cache_read: 0.1 },
          modalities: { input: ['text', 'image'], output: ['text'] },
          canonical_model_id: 'acme/free-model',
        },
      },
    },
  },
}

describe('parseModelsDevCatalog', () => {
  it('maps providers and models while preserving a zero token cost as a model fact', () => {
    const catalog = parseModelsDevCatalog(source)

    expect(catalog.providers).toHaveLength(1)
    expect(catalog.providers[0]).toMatchObject({
      providerId: 'acme',
      name: 'Acme',
      envKeys: ['ACME_API_KEY'],
      sourceRecord: 'acme',
      retained: false,
    })
    expect(catalog.providers[0]).not.toHaveProperty('verifiedAt')
    expect(catalog.models).toHaveLength(1)
    expect(catalog.models[0]).toMatchObject({
      providerId: 'acme',
      modelId: 'free-model',
      canonicalModelId: 'acme/free-model',
      familyId: 'acme',
      costInput: 0,
      costOutput: 0,
      costCacheRead: 0.1,
      contextLimit: 128000,
      inputLimit: 120000,
      outputLimit: 8192,
      modalitiesInput: ['text', 'image'],
      sourceRecord: 'acme/free-model',
    })
    expect(catalog.knownGaps.length).toBeGreaterThan(0)
  })

  it('keeps a provider with no models', () => {
    const catalog = parseModelsDevCatalog({ providers: { empty: { id: 'empty', name: 'Empty' } } })

    expect(catalog.providers).toHaveLength(1)
    expect(catalog.models).toHaveLength(0)
  })

  it('accepts the provider-map fallback shape', () => {
    const catalog = parseModelsDevCatalog(
      { acme: { id: 'acme', name: 'Acme', models: {} } },
      'https://models.dev/api.json?type=all',
    )

    expect(catalog.providers[0]?.sourceUrl).toBe('https://models.dev/api.json?type=all')
  })

  it('rejects a response that is not a provider map', () => {
    expect(() => parseModelsDevCatalog({ providers: [] })).toThrow('not an object')
  })

  it('correlates the same release served by several providers on one canonical model', () => {
    const catalog = parseModelsDevCatalog({
      providers: {
        deepseek: {
          id: 'deepseek',
          name: 'DeepSeek',
          models: {
            'deepseek-v4.1-0731': {
              id: 'deepseek-v4.1-0731',
              name: 'DeepSeek V4.1 0731',
              canonical_model_id: 'deepseek/deepseek-v4.1-0731',
            },
          },
        },
        gateway: {
          id: 'gateway',
          name: 'Gateway',
          models: {
            'deepseek-ai/DeepSeek-V4.1-0731': {
              id: 'deepseek-ai/DeepSeek-V4.1-0731',
              name: 'DeepSeek V4.1',
              canonical_model_id: 'deepseek/deepseek-v4.1-0731',
            },
          },
        },
      },
    })

    expect(catalog.models.map((model) => model.canonicalModelId)).toEqual([
      'deepseek/deepseek-v4-1-0731',
      'deepseek/deepseek-v4-1-0731',
    ])
    expect(catalog.models[0]?.familyId).toBe('deepseek-v4-1-0731')
  })

  it('infers parents of parents from the family tree', () => {
    expect(familyChain('opus-5-5').map((family) => family.name)).toEqual([
      'Opus 5.5',
      'Opus 5',
      'Opus',
      'Claude',
    ])
    expect(familyChain('deepseek-v4-1-0731').map((family) => family.familyId)).toEqual([
      'deepseek-v4-1-0731',
      'deepseek-v4-1',
      'deepseek-v4',
      'deepseek',
    ])
  })
})
