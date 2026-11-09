import { createHash } from 'node:crypto'
import { canonicalModelIdFor, familyIdForCanonical } from './curated'
import type {
  CatalogModel,
  CatalogProvider,
  CatalogSource,
  ModelsDevCatalog,
  ModelsDevFetchResult,
} from './types'

export const MODELS_DEV_SOURCE: CatalogSource = {
  sourceId: 'models-dev',
  name: 'models.dev',
  sourceUrl: 'https://models.dev/catalog.json?type=all',
  license: 'MIT',
  attribution: 'models.dev (https://models.dev), copyright 2025 models.dev, MIT License',
}

export const MODELS_DEV_KNOWN_GAPS = [
  'models.dev does not provide coding-agent harness compatibility.',
  'models.dev does not provide subscription plans, prices or usage models.',
  'models.dev does not provide recurring free allowances or reset periods.',
  'models.dev does not provide local runtime or hardware requirements.',
  'models.dev does not provide benchmark scores.',
]

const USER_AGENT = 'providers-catalog-web/0.2 (+https://models.dev)'
const DEFAULT_TIMEOUT_MS = 60_000
const FALLBACK_URL = 'https://models.dev/api.json?type=all'

export function parseModelsDevCatalog(
  value: unknown,
  sourceUrl: string = MODELS_DEV_SOURCE.sourceUrl,
): ModelsDevCatalog {
  const root = asRecord(value)
  if (!root) {
    throw new Error('models.dev response is not an object')
  }

  // The combined catalog wraps providers in `providers`; the provider-map
  // endpoint is the same provider objects at the root. Accept both so the
  // fallback does not need a second parser.
  const providerMap = asRecord(root.providers) ?? root

  const providers: CatalogProvider[] = []
  const models: CatalogModel[] = []

  for (const [providerKey, providerValue] of Object.entries(providerMap)) {
    const provider = asRecord(providerValue)
    if (!provider) {
      throw new Error(`models.dev provider ${providerKey} is not an object`)
    }

    const providerId = optionalString(provider, 'id') ?? providerKey
    const providerName = optionalString(provider, 'name') ?? providerId
    const modelMap = asRecord(provider.models) ?? {}

    providers.push({
      providerId,
      name: providerName,
      npm: optionalString(provider, 'npm'),
      apiUrl: optionalString(provider, 'api'),
      docUrl: optionalString(provider, 'doc'),
      envKeys: stringArray(provider.env) ?? [],
      logoPath: null,
      retained: false,
      sourceId: MODELS_DEV_SOURCE.sourceId,
      sourceUrl,
      sourceRecord: providerKey,
    })

    for (const [modelKey, modelValue] of Object.entries(modelMap)) {
      const model = asRecord(modelValue)
      if (!model) {
        throw new Error(`models.dev model ${providerKey}/${modelKey} is not an object`)
      }
      models.push(mapModel(providerId, providerKey, modelKey, model, sourceUrl))
    }
  }

  return {
    providers,
    models,
    knownGaps: [...MODELS_DEV_KNOWN_GAPS],
  }
}

export async function fetchModelsDevCatalog(options: {
  fetchImpl?: typeof fetch
  url?: string
  timeoutMs?: number
} = {}): Promise<ModelsDevFetchResult> {
  const fetchImpl = options.fetchImpl ?? fetch
  const primaryUrl = options.url ?? MODELS_DEV_SOURCE.sourceUrl
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS

  try {
    return await fetchOne(fetchImpl, primaryUrl, timeoutMs)
  } catch (primaryError) {
    if (primaryUrl !== MODELS_DEV_SOURCE.sourceUrl) {
      throw primaryError
    }
    try {
      return await fetchOne(fetchImpl, FALLBACK_URL, timeoutMs)
    } catch (fallbackError) {
      throw new AggregateError(
        [primaryError, fallbackError],
        'models.dev catalog and provider-map endpoints both failed',
      )
    }
  }
}

async function fetchOne(
  fetchImpl: typeof fetch,
  url: string,
  timeoutMs: number,
): Promise<ModelsDevFetchResult> {
  const response = await fetchImpl(url, {
    headers: { 'user-agent': USER_AGENT },
    signal: AbortSignal.timeout(timeoutMs),
  })
  const body = await response.text()

  if (!response.ok) {
    throw new Error(`models.dev returned HTTP ${response.status}: ${body.slice(0, 200)}`)
  }

  let value: unknown
  try {
    value = JSON.parse(body)
  } catch (error) {
    throw new Error(`models.dev returned invalid JSON: ${errorMessage(error)}`)
  }

  const fetchedAt = new Date().toISOString()
  return {
    catalog: parseModelsDevCatalog(value, url),
    sourceUrl: url,
    fetchedAt,
    contentSha256: createHash('sha256').update(body).digest('hex'),
    httpStatus: response.status,
    etag: response.headers.get('etag'),
  }
}

function mapModel(
  providerId: string,
  providerKey: string,
  modelKey: string,
  model: Record<string, unknown>,
  sourceUrl: string,
): CatalogModel {
  const limits = asRecord(model.limit) ?? {}
  const costs = asRecord(model.cost) ?? {}
  const modalities = asRecord(model.modalities) ?? {}
  const modelId = optionalString(model, 'id') ?? modelKey
  const canonicalModelId = canonicalModelIdFor(
    providerId,
    modelId,
    optionalString(model, 'canonical_model_id'),
  )

  return {
    providerId,
    modelId,
    modelType: optionalString(model, 'type'),
    canonicalModelId,
    name: optionalString(model, 'name'),
    family: optionalString(model, 'family'),
    familyId: familyIdForCanonical(canonicalModelId) ?? canonicalModelId.split('/')[0],
    description: optionalString(model, 'description'),
    releaseDate: optionalString(model, 'release_date'),
    lastUpdated: optionalString(model, 'last_updated'),
    knowledgeDate: optionalString(model, 'knowledge'),
    openWeights: optionalBoolean(model, 'open_weights'),
    attachment: optionalBoolean(model, 'attachment'),
    reasoning: optionalBoolean(model, 'reasoning'),
    toolCall: optionalBoolean(model, 'tool_call'),
    structuredOutput: optionalBoolean(model, 'structured_output'),
    temperature: optionalBoolean(model, 'temperature'),
    interleaved: jsonString(model.interleaved),
    contextLimit: optionalNumber(limits, 'context'),
    inputLimit: optionalNumber(limits, 'input'),
    outputLimit: optionalNumber(limits, 'output'),
    costInput: optionalNumber(costs, 'input'),
    costOutput: optionalNumber(costs, 'output'),
    costReasoning: optionalNumber(costs, 'reasoning'),
    costCacheRead: optionalNumber(costs, 'cache_read'),
    costCacheWrite: optionalNumber(costs, 'cache_write'),
    costInputAudio: optionalNumber(costs, 'input_audio'),
    costOutputAudio: optionalNumber(costs, 'output_audio'),
    modalitiesInput: stringArray(modalities.input),
    modalitiesOutput: stringArray(modalities.output),
    status: optionalString(model, 'status'),
    retained: false,
    sourceId: MODELS_DEV_SOURCE.sourceId,
    sourceUrl,
    sourceRecord: `${providerKey}/${modelKey}`,
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null
  }
  return value as Record<string, unknown>
}

function optionalString(record: Record<string, unknown>, key: string): string | null {
  const value = record[key]
  return typeof value === 'string' ? value : null
}

function optionalNumber(record: Record<string, unknown>, key: string): number | null {
  const value = record[key]
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value
  }
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) {
      return parsed
    }
  }
  return null
}

function optionalBoolean(record: Record<string, unknown>, key: string): boolean | null {
  const value = record[key]
  return typeof value === 'boolean' ? value : null
}

function stringArray(value: unknown): string[] | null {
  if (!Array.isArray(value)) {
    return null
  }
  return value.filter((item): item is string => typeof item === 'string')
}

function jsonString(value: unknown): string | null {
  if (value === undefined || value === null) {
    return null
  }
  return JSON.stringify(value)
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
