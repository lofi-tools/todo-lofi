import path from 'node:path'
import { DuckDBInstance, type DuckDBConnection, type DuckDBValue } from '@duckdb/node-api'
import { SCHEMA_VERSION } from '../schema'
import { findLatestSnapshot, freshnessFor } from '../snapshot'
import type { Confidence, ScrapeStatus } from '../types'

export interface CatalogSource {
  sourceId: string
  name: string
  sourceUrl: string
  license: string
  attribution: string
  fetchedAt: string
  httpStatus: number | null
  etag: string | null
  contentSha256: string
  status: string
  error: string | null
}

export interface CatalogSnapshot {
  snapshotId: string
  schemaVersion: number
  generatedAt: string
  sourceFetchedAt: string
  freshnessDeadline: string
  stale: boolean
  ageDays: number
  license: string
  attribution: string
  sourceUrls: Record<string, string>
  sources: CatalogSource[]
  knownGaps: string[]
}

export interface CatalogProvider {
  providerId: string
  name: string
  npm: string | null
  apiUrl: string | null
  docUrl: string | null
  envKeys: string[]
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
  modelCount: number
  openWeightCount: number
  planCount: number
  offerCount: number
  localRuntimeCount: number
}

export interface CatalogModel {
  providerId: string
  providerName: string
  modelId: string
  modelType: string | null
  canonicalModelId: string | null
  name: string | null
  family: string | null
  description: string | null
  releaseDate: string | null
  lastUpdated: string | null
  knowledgeDate: string | null
  openWeights: boolean | null
  attachment: boolean | null
  reasoning: boolean | null
  toolCall: boolean | null
  structuredOutput: boolean | null
  temperature: boolean | null
  interleaved: string | null
  contextLimit: number | null
  inputLimit: number | null
  outputLimit: number | null
  costInput: number | null
  costOutput: number | null
  costReasoning: number | null
  costCacheRead: number | null
  costCacheWrite: number | null
  costInputAudio: number | null
  costOutputAudio: number | null
  modalitiesInput: string[] | null
  modalitiesOutput: string[] | null
  status: string | null
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
}

export interface CatalogPlan {
  planId: string
  providerId: string
  providerName: string
  name: string
  usageModel: string
  billingPeriod: string | null
  price: number | null
  currency: string | null
  includedAllowance: number | null
  allowanceUnit: string | null
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
}

export interface CatalogOffer {
  offerId: string
  providerId: string
  providerName: string
  planId: string | null
  offerKind: string
  allowance: number
  allowanceUnit: string
  resetPeriod: string
  eligibility: string | null
  conditions: string | null
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
}

export interface CatalogAgent {
  agentId: string
  name: string
  vendor: string | null
  homepageUrl: string | null
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
}

export interface CatalogLocalRuntime {
  providerId: string
  modelId: string
  runtime: string
  quantization: string | null
  approximateHardware: string | null
  setupNotes: string | null
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
}

export interface CatalogData {
  snapshotPath: string
  snapshot: CatalogSnapshot
  providers: CatalogProvider[]
  models: CatalogModel[]
  plans: CatalogPlan[]
  offers: CatalogOffer[]
  agents: CatalogAgent[]
  localRuntimes: CatalogLocalRuntime[]
}

let cachedCatalog: Promise<CatalogData> | null = null

export function loadCatalog(): Promise<CatalogData> {
  cachedCatalog ??= readCatalog()
  return cachedCatalog
}

async function readCatalog(): Promise<CatalogData> {
  const configuredPath = process.env.CATALOG_DUCKDB_PATH
  const snapshotPath = configuredPath
    ? path.resolve(configuredPath)
    : await findLatestSnapshot(path.resolve(process.cwd(), 'artifacts'))

  if (!snapshotPath) {
    throw new Error(
      'No catalog snapshot found. Run `pnpm --dir apps/providers-catalog-web run snapshot` or set CATALOG_DUCKDB_PATH.',
    )
  }

  const instance = await DuckDBInstance.create(snapshotPath, { access_mode: 'READ_ONLY' })
  const connection = await instance.connect()

  try {
    const metaRows = await readRows(
      connection,
      `SELECT snapshot_id, schema_version, epoch_ms(generated_at) AS generated_at_ms,
              epoch_ms(source_fetched_at) AS source_fetched_at_ms,
              epoch_ms(freshness_deadline) AS freshness_deadline_ms,
              license, attribution, source_urls, known_gaps
       FROM catalog_meta`,
    )
    if (metaRows.length !== 1) {
      throw new Error(`catalog_meta must contain exactly one row, found ${metaRows.length}`)
    }
    const meta = metaRows[0]
    const schemaVersion = numberValue(meta.schema_version)
    if (schemaVersion !== SCHEMA_VERSION) {
      throw new Error(
        `Unsupported catalog schema version ${schemaVersion}; this site reads schema ${SCHEMA_VERSION}.`,
      )
    }
    const generatedAt = new Date(numberValue(meta.generated_at_ms)).toISOString()
    const freshness = freshnessFor(generatedAt)
    const freshnessDeadline = new Date(numberValue(meta.freshness_deadline_ms)).toISOString()

    const sources = (
      await readRows(
        connection,
        `SELECT source_id, name, source_url, license, attribution,
                epoch_ms(fetched_at) AS fetched_at_ms, http_status, etag,
                content_sha256, status, error
         FROM sources ORDER BY source_id`,
      )
    ).map(mapSource)
    if (sources.length === 0) {
      throw new Error('The snapshot has no sources rows, so it cannot publish provenance.')
    }

    const models = (
      await readRows(
        connection,
        `SELECT m.provider_id, p.name AS provider_name, m.model_id, m.model_type,
                m.canonical_model_id, m.name, m.family, m.description, m.release_date,
                m.last_updated, m.knowledge_date, m.open_weights, m.attachment,
                m.reasoning, m.tool_call, m.structured_output, m.temperature,
                m.interleaved, CAST(m.context_limit AS DOUBLE) AS context_limit,
                CAST(m.input_limit AS DOUBLE) AS input_limit,
                CAST(m.output_limit AS DOUBLE) AS output_limit,
                CAST(m.cost_input AS DOUBLE) AS cost_input,
                CAST(m.cost_output AS DOUBLE) AS cost_output,
                CAST(m.cost_reasoning AS DOUBLE) AS cost_reasoning,
                CAST(m.cost_cache_read AS DOUBLE) AS cost_cache_read,
                CAST(m.cost_cache_write AS DOUBLE) AS cost_cache_write,
                CAST(m.cost_input_audio AS DOUBLE) AS cost_input_audio,
                CAST(m.cost_output_audio AS DOUBLE) AS cost_output_audio,
                m.modalities_input, m.modalities_output, m.status,
                m.source_id, m.source_url, m.source_record,
                epoch_ms(m.verified_at) AS verified_at_ms,
                m.confidence, m.scrape_status
         FROM models m
         JOIN providers p ON p.provider_id = m.provider_id
         ORDER BY lower(p.name), lower(coalesce(m.name, m.model_id)), m.model_id`,
      )
    ).map(mapModel)

    const plans = (
      await readRows(
        connection,
        `SELECT pl.plan_id, pl.provider_id, pr.name AS provider_name, pl.name,
                pl.usage_model, pl.billing_period, CAST(pl.price AS DOUBLE) AS price,
                pl.currency, CAST(pl.included_allowance AS DOUBLE) AS included_allowance,
                pl.allowance_unit, pl.source_id, pl.source_url, pl.source_record,
                epoch_ms(pl.verified_at) AS verified_at_ms, pl.confidence, pl.scrape_status
         FROM plans pl
         JOIN providers pr ON pr.provider_id = pl.provider_id
         ORDER BY lower(pr.name), lower(pl.name)`,
      )
    ).map(mapPlan)

    const offers = (
      await readRows(
        connection,
        `SELECT o.offer_id, o.provider_id, pr.name AS provider_name, o.plan_id,
                o.offer_kind, CAST(o.allowance AS DOUBLE) AS allowance, o.allowance_unit,
                o.reset_period, o.eligibility, o.conditions, o.source_id, o.source_url,
                o.source_record,
                epoch_ms(o.verified_at) AS verified_at_ms, o.confidence, o.scrape_status
         FROM free_offers o
         JOIN providers pr ON pr.provider_id = o.provider_id
         ORDER BY lower(pr.name), lower(o.offer_id)`,
      )
    ).map(mapOffer)

    const agents = (
      await readRows(
        connection,
        `SELECT agent_id, name, vendor, homepage_url, source_id, source_url, source_record,
                epoch_ms(verified_at) AS verified_at_ms, confidence, scrape_status
         FROM agents ORDER BY lower(name)`,
      )
    ).map(mapAgent)

    const localRuntimes = (
      await readRows(
        connection,
        `SELECT provider_id, model_id, runtime, quantization, approximate_hardware,
                setup_notes, source_id, source_url, source_record,
                epoch_ms(verified_at) AS verified_at_ms, confidence, scrape_status
         FROM local_runtimes ORDER BY provider_id, model_id, runtime`,
      )
    ).map(mapLocalRuntime)

    const providers = (
      await readRows(
        connection,
        `SELECT provider_id, name, npm, api_url, doc_url, env_keys, source_id, source_url,
                source_record, epoch_ms(verified_at) AS verified_at_ms, confidence, scrape_status
         FROM providers ORDER BY lower(name), provider_id`,
      )
    ).map((row) => mapProvider(row, models, plans, offers, localRuntimes))

    return {
      snapshotPath,
      snapshot: {
        snapshotId: stringValue(meta.snapshot_id),
        schemaVersion,
        generatedAt,
        sourceFetchedAt: isoFromMs(meta.source_fetched_at_ms),
        freshnessDeadline,
        stale: freshness.stale,
        ageDays: freshness.ageDays,
        license: stringValue(meta.license),
        attribution: stringValue(meta.attribution),
        sourceUrls: jsonObject(meta.source_urls),
        sources,
        knownGaps: jsonStringArray(meta.known_gaps),
      },
      providers,
      models,
      plans,
      offers,
      agents,
      localRuntimes,
    }
  } finally {
    connection.closeSync()
    instance.closeSync()
  }
}

function mapProvider(
  row: Record<string, DuckDBValue>,
  models: CatalogModel[],
  plans: CatalogPlan[],
  offers: CatalogOffer[],
  localRuntimes: CatalogLocalRuntime[],
): CatalogProvider {
  const providerId = stringValue(row.provider_id)
  const providerModels = models.filter((model) => model.providerId === providerId)
  return {
    providerId,
    name: stringValue(row.name),
    npm: nullableString(row.npm),
    apiUrl: nullableString(row.api_url),
    docUrl: nullableString(row.doc_url),
    envKeys: jsonStringArray(row.env_keys),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
    modelCount: providerModels.length,
    openWeightCount: providerModels.filter((model) => model.openWeights === true).length,
    planCount: plans.filter((plan) => plan.providerId === providerId).length,
    offerCount: offers.filter((offer) => offer.providerId === providerId).length,
    localRuntimeCount: localRuntimes.filter((runtime) => runtime.providerId === providerId).length,
  }
}

function mapModel(row: Record<string, DuckDBValue>): CatalogModel {
  return {
    providerId: stringValue(row.provider_id),
    providerName: stringValue(row.provider_name),
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
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
  }
}

function mapSource(row: Record<string, DuckDBValue>): CatalogSource {
  return {
    sourceId: stringValue(row.source_id),
    name: stringValue(row.name),
    sourceUrl: stringValue(row.source_url),
    license: stringValue(row.license),
    attribution: stringValue(row.attribution),
    fetchedAt: isoFromMs(row.fetched_at_ms),
    httpStatus: nullableNumber(row.http_status),
    etag: nullableString(row.etag),
    contentSha256: stringValue(row.content_sha256),
    status: stringValue(row.status),
    error: nullableString(row.error),
  }
}

function mapPlan(row: Record<string, DuckDBValue>): CatalogPlan {
  return {
    planId: stringValue(row.plan_id),
    providerId: stringValue(row.provider_id),
    providerName: stringValue(row.provider_name),
    name: stringValue(row.name),
    usageModel: stringValue(row.usage_model),
    billingPeriod: nullableString(row.billing_period),
    price: nullableNumber(row.price),
    currency: nullableString(row.currency),
    includedAllowance: nullableNumber(row.included_allowance),
    allowanceUnit: nullableString(row.allowance_unit),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
  }
}

function mapOffer(row: Record<string, DuckDBValue>): CatalogOffer {
  return {
    offerId: stringValue(row.offer_id),
    providerId: stringValue(row.provider_id),
    providerName: stringValue(row.provider_name),
    planId: nullableString(row.plan_id),
    offerKind: stringValue(row.offer_kind),
    allowance: numberValue(row.allowance),
    allowanceUnit: stringValue(row.allowance_unit),
    resetPeriod: stringValue(row.reset_period),
    eligibility: nullableString(row.eligibility),
    conditions: nullableString(row.conditions),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
  }
}

function mapAgent(row: Record<string, DuckDBValue>): CatalogAgent {
  return {
    agentId: stringValue(row.agent_id),
    name: stringValue(row.name),
    vendor: nullableString(row.vendor),
    homepageUrl: nullableString(row.homepage_url),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
  }
}

function mapLocalRuntime(row: Record<string, DuckDBValue>): CatalogLocalRuntime {
  return {
    providerId: stringValue(row.provider_id),
    modelId: stringValue(row.model_id),
    runtime: stringValue(row.runtime),
    quantization: nullableString(row.quantization),
    approximateHardware: nullableString(row.approximate_hardware),
    setupNotes: nullableString(row.setup_notes),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
    verifiedAt: isoFromMs(row.verified_at_ms),
    confidence: confidenceValue(row.confidence),
    scrapeStatus: scrapeStatusValue(row.scrape_status),
  }
}

async function readRows(
  connection: DuckDBConnection,
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
  try {
    return numberValue(value)
  } catch {
    return null
  }
}

function nullableBoolean(value: DuckDBValue): boolean | null {
  return typeof value === 'boolean' ? value : null
}

function isoFromMs(value: DuckDBValue): string {
  return new Date(numberValue(value)).toISOString()
}

function jsonStringArray(value: DuckDBValue): string[] {
  if (typeof value !== 'string') {
    return []
  }
  try {
    const parsed: unknown = JSON.parse(value)
    return Array.isArray(parsed)
      ? parsed.filter((item): item is string => typeof item === 'string')
      : []
  } catch {
    return []
  }
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

function jsonObject(value: DuckDBValue): Record<string, string> {
  if (typeof value !== 'string') {
    return {}
  }
  try {
    const parsed: unknown = JSON.parse(value)
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
      return {}
    }
    return Object.fromEntries(
      Object.entries(parsed).filter((entry): entry is [string, string] => typeof entry[1] === 'string'),
    )
  } catch {
    return {}
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
