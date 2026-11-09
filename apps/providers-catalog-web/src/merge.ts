import { existsSync } from 'node:fs'
import { DuckDBInstance, type DuckDBValue } from '@duckdb/node-api'
import { SCHEMA_VERSION } from './schema'
import type { CatalogModel, CatalogProvider, Confidence, ModelsDevCatalog, ScrapeStatus } from './types'

export interface MergeReport {
  previousSnapshotPath: string
  attemptedAt: string
  retainedProviders: string[]
  retainedModels: string[]
  reviewItems: string[]
}

export interface MergeResult {
  catalog: ModelsDevCatalog
  report: MergeReport
}

/**
 * A successful scrape can omit a fact because the source removed it or because
 * the response is incomplete. Keep the last verified row and mark it missing
 * instead of silently deleting it.
 */
export async function mergePreviousFacts(
  catalog: ModelsDevCatalog,
  previousSnapshotPath: string,
  attemptedAt: string,
): Promise<MergeResult> {
  if (!existsSync(previousSnapshotPath)) {
    throw new Error(`Previous catalog snapshot does not exist: ${previousSnapshotPath}`)
  }

  const instance = await DuckDBInstance.create(previousSnapshotPath, { access_mode: 'READ_ONLY' })
  const connection = await instance.connect()

  try {
    const metaRows = await readRows(connection, 'SELECT schema_version FROM catalog_meta')
    if (metaRows.length !== 1) {
      throw new Error(`Previous catalog_meta must contain exactly one row, found ${metaRows.length}`)
    }
    const schemaVersion = numberValue(metaRows[0].schema_version)
    if (schemaVersion !== SCHEMA_VERSION) {
      throw new Error(
        `Cannot merge snapshot schema ${schemaVersion}; expected ${SCHEMA_VERSION}.`,
      )
    }

    const previousProviders = (
      await readRows(
        connection,
        `SELECT provider_id, name, npm, api_url, doc_url, env_keys, logo_path,
                source_id, source_url, source_record, epoch_ms(verified_at) AS verified_at_ms,
                epoch_ms(last_attempt_at) AS last_attempt_at_ms, confidence,
                scrape_status, last_error
         FROM providers`,
      )
    ).map(mapProvider)
    const previousModels = (
      await readRows(
        connection,
        `SELECT provider_id, model_id, model_type, canonical_model_id, name, family,
                description, release_date, last_updated, knowledge_date, open_weights,
                attachment, reasoning, tool_call, structured_output, temperature,
                interleaved, CAST(context_limit AS DOUBLE) AS context_limit,
                CAST(input_limit AS DOUBLE) AS input_limit,
                CAST(output_limit AS DOUBLE) AS output_limit,
                CAST(cost_input AS DOUBLE) AS cost_input,
                CAST(cost_output AS DOUBLE) AS cost_output,
                CAST(cost_reasoning AS DOUBLE) AS cost_reasoning,
                CAST(cost_cache_read AS DOUBLE) AS cost_cache_read,
                CAST(cost_cache_write AS DOUBLE) AS cost_cache_write,
                CAST(cost_input_audio AS DOUBLE) AS cost_input_audio,
                CAST(cost_output_audio AS DOUBLE) AS cost_output_audio,
                modalities_input, modalities_output, status, source_id, source_url,
                source_record, epoch_ms(verified_at) AS verified_at_ms,
                epoch_ms(last_attempt_at) AS last_attempt_at_ms, confidence,
                scrape_status, last_error
         FROM models`,
      )
    ).map(mapModel)

    const currentProviderIds = new Set(catalog.providers.map((provider) => provider.providerId))
    const currentModelKeys = new Set(catalog.models.map((model) => modelKey(model.providerId, model.modelId)))

    const retainedProviders: CatalogProvider[] = previousProviders
      .filter((provider) => !currentProviderIds.has(provider.providerId))
      .map((provider) => ({
        ...provider,
        lastAttemptAt: attemptedAt,
        scrapeStatus: 'missing',
        lastError: 'Not present in the latest source response',
      }))
    const retainedModels: CatalogModel[] = previousModels
      .filter((model) => !currentModelKeys.has(modelKey(model.providerId, model.modelId)))
      .map((model) => ({
        ...model,
        lastAttemptAt: attemptedAt,
        scrapeStatus: 'missing',
        lastError: 'Not present in the latest source response',
      }))

    const reviewItems = [
      ...retainedProviders.map((provider) => `provider ${provider.providerId}`),
      ...retainedModels.map((model) => `model ${model.providerId}/${model.modelId}`),
    ].sort()

    return {
      catalog: {
        ...catalog,
        providers: [...catalog.providers, ...retainedProviders],
        models: [...catalog.models, ...retainedModels],
      },
      report: {
        previousSnapshotPath,
        attemptedAt,
        retainedProviders: retainedProviders.map((provider) => provider.providerId).sort(),
        retainedModels: retainedModels.map((model) => modelKey(model.providerId, model.modelId)).sort(),
        reviewItems,
      },
    }
  } finally {
    connection.closeSync()
    instance.closeSync()
  }
}

function modelKey(providerId: string, modelId: string): string {
  return `${providerId}/${modelId}`
}

function mapProvider(row: Record<string, DuckDBValue>): CatalogProvider {
  return {
    providerId: stringValue(row.provider_id),
    name: stringValue(row.name),
    npm: nullableString(row.npm),
    apiUrl: nullableString(row.api_url),
    docUrl: nullableString(row.doc_url),
    envKeys: jsonStringArray(row.env_keys),
    logoPath: nullableString(row.logo_path),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    lastAttemptAt: isoFromMs(row.last_attempt_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
    lastError: nullableString(row.last_error),
  }
}

function mapModel(row: Record<string, DuckDBValue>): CatalogModel {
  return {
    providerId: stringValue(row.provider_id),
    modelId: stringValue(row.model_id),
    modelType: nullableString(row.model_type),
    canonicalModelId: nullableString(row.canonical_model_id),
    name: nullableString(row.name),
    family: nullableString(row.family),
    description: nullableString(row.description),
    releaseDate: nullableString(row.release_date),
    lastUpdated: nullableString(row.last_updated),
    knowledgeDate: nullableString(row.knowledge_date),
    openWeights: nullableBoolean(row.open_weights),
    attachment: nullableBoolean(row.attachment),
    reasoning: nullableBoolean(row.reasoning),
    toolCall: nullableBoolean(row.tool_call),
    structuredOutput: nullableBoolean(row.structured_output),
    temperature: nullableBoolean(row.temperature),
    interleaved: nullableString(row.interleaved),
    contextLimit: nullableNumber(row.context_limit),
    inputLimit: nullableNumber(row.input_limit),
    outputLimit: nullableNumber(row.output_limit),
    costInput: nullableNumber(row.cost_input),
    costOutput: nullableNumber(row.cost_output),
    costReasoning: nullableNumber(row.cost_reasoning),
    costCacheRead: nullableNumber(row.cost_cache_read),
    costCacheWrite: nullableNumber(row.cost_cache_write),
    costInputAudio: nullableNumber(row.cost_input_audio),
    costOutputAudio: nullableNumber(row.cost_output_audio),
    modalitiesInput: nullableJsonStringArray(row.modalities_input),
    modalitiesOutput: nullableJsonStringArray(row.modalities_output),
    status: nullableString(row.status),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    lastAttemptAt: isoFromMs(row.last_attempt_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
    lastError: nullableString(row.last_error),
  }
}

async function readRows(
  connection: Awaited<ReturnType<DuckDBInstance['connect']>>,
  sql: string,
): Promise<Record<string, DuckDBValue>[]> {
  const reader = await connection.runAndReadAll(sql)
  return reader.getRowObjects()
}

function stringValue(value: DuckDBValue): string {
  if (typeof value === 'string') {
    return value
  }
  throw new Error(`Expected a string, got ${String(value)}`)
}

function nullableString(value: DuckDBValue): string | null {
  return typeof value === 'string' ? value : null
}

function numberValue(value: DuckDBValue): number {
  if (typeof value === 'number') {
    return value
  }
  if (typeof value === 'bigint') {
    return Number(value)
  }
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) {
      return parsed
    }
  }
  throw new Error(`Expected a number, got ${String(value)}`)
}

function nullableNumber(value: DuckDBValue): number | null {
  if (value === null || value === undefined) {
    return null
  }
  return numberValue(value)
}

function nullableBoolean(value: DuckDBValue): boolean | null {
  return typeof value === 'boolean' ? value : null
}

function isoFromMs(value: DuckDBValue): string {
  return new Date(numberValue(value)).toISOString()
}

function jsonStringArray(value: DuckDBValue): string[] {
  return nullableJsonStringArray(value) ?? []
}

function nullableJsonStringArray(value: DuckDBValue): string[] | null {
  if (typeof value !== 'string') {
    return null
  }
  try {
    const parsed: unknown = JSON.parse(value)
    return Array.isArray(parsed)
      ? parsed.filter((item): item is string => typeof item === 'string')
      : null
  } catch {
    return null
  }
}

function confidenceValue(value: DuckDBValue): Confidence {
  if (value === 'verified' || value === 'community' || value === 'inferred' || value === 'unknown') {
    return value
  }
  return 'unknown'
}

function scrapeStatusValue(value: DuckDBValue): ScrapeStatus {
  if (value === 'ok' || value === 'missing' || value === 'failed' || value === 'not_fetched') {
    return value
  }
  return 'not_fetched'
}
