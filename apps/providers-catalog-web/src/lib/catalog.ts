import path from 'node:path'
import { DuckDBInstance, type DuckDBConnection, type DuckDBValue } from '@duckdb/node-api'
import { SCHEMA_VERSION } from '../schema'
import { findLatestSnapshot, freshnessFor } from '../snapshot'
import type {
  BenchmarkUnit,
  CatalogBenchmark,
  CatalogBenchmarkScore,
  CatalogCanonicalModel,
  CatalogCompany,
  CatalogCompanyPlan,
  CatalogFamily,
  CatalogHarness,
  CatalogProviderHarness,
} from '../types'

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
  retained: boolean
  sourceId: string
  sourceUrl: string
  sourceRecord: string
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
  canonicalModelId: string
  canonicalModelName: string
  familyId: string
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
  retained: boolean
  sourceId: string
  sourceUrl: string
  sourceRecord: string
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
}

export interface CatalogData {
  snapshotPath: string
  snapshot: CatalogSnapshot
  providers: CatalogProvider[]
  models: CatalogModel[]
  canonicalModels: CatalogCanonicalModel[]
  families: CatalogFamily[]
  companies: CatalogCompany[]
  companyPlans: CatalogCompanyPlan[]
  harnesses: CatalogHarness[]
  providerHarnesses: CatalogProviderHarness[]
  benchmarks: CatalogBenchmark[]
  benchmarkScores: CatalogBenchmarkScore[]
  plans: CatalogPlan[]
  offers: CatalogOffer[]
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

    const canonicalModels = (
      await readRows(
        connection,
        `SELECT canonical_model_id, name, lab, family_id
         FROM canonical_models ORDER BY canonical_model_id`,
      )
    ).map(mapCanonicalModel)

    const families = (
      await readRows(
        connection,
        `SELECT family_id, name, parent_family_id, vendor
         FROM model_families ORDER BY family_id`,
      )
    ).map(mapFamily)

    const companies = (
      await readRows(
        connection,
        `SELECT company_id, name, homepage_url, source_url
         FROM companies ORDER BY lower(name), company_id`,
      )
    ).map(mapCompany)

    const companyPlans = (
      await readRows(
        connection,
        `SELECT plan_id, company_id, name, provider_id, source_url
         FROM company_plans ORDER BY lower(name), plan_id`,
      )
    ).map(mapCompanyPlan)

    const harnesses = (
      await readRows(
        connection,
        `SELECT harness_id, name, vendor, homepage_url
         FROM harnesses ORDER BY name`,
      )
    ).map(mapHarness)

    const providerHarnesses = (
      await readRows(
        connection,
        `SELECT provider_id, harness_id, exclusive, source_url
         FROM provider_harnesses ORDER BY provider_id, harness_id`,
      )
    ).map(mapProviderHarness)

    const benchmarks = (
      await readRows(
        connection,
        `SELECT benchmark_id, name, description, unit, higher_is_better, source_url
         FROM benchmarks ORDER BY name`,
      )
    ).map(mapBenchmark)

    const benchmarkScores = (
      await readRows(
        connection,
        `SELECT canonical_model_id, benchmark_id, CAST(score AS DOUBLE) AS score, source_url
         FROM model_benchmarks ORDER BY canonical_model_id, benchmark_id`,
      )
    ).map(mapBenchmarkScore)

    const models = (
      await readRows(
        connection,
        `SELECT m.provider_id, p.name AS provider_name, m.model_id, m.model_type,
                m.canonical_model_id, c.name AS canonical_model_name,
                c.family_id, m.name, m.family, m.description, m.release_date,
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
                m.modalities_input, m.modalities_output, m.status, m.retained,
                m.source_id, m.source_url, m.source_record
         FROM models m
         JOIN providers p ON p.provider_id = m.provider_id
         JOIN canonical_models c ON c.canonical_model_id = m.canonical_model_id
         ORDER BY lower(p.name), lower(coalesce(m.name, m.model_id)), m.model_id`,
      )
    ).map(mapModel)

    const plans = (
      await readRows(
        connection,
        `SELECT pl.plan_id, pl.provider_id, pr.name AS provider_name, pl.name,
                pl.usage_model, pl.billing_period, CAST(pl.price AS DOUBLE) AS price,
                pl.currency, CAST(pl.included_allowance AS DOUBLE) AS included_allowance,
                pl.allowance_unit, pl.source_id, pl.source_url, pl.source_record
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
                o.source_record
         FROM free_offers o
         JOIN providers pr ON pr.provider_id = o.provider_id
         ORDER BY lower(pr.name), lower(o.offer_id)`,
      )
    ).map(mapOffer)

    const localRuntimes = (
      await readRows(
        connection,
        `SELECT provider_id, model_id, runtime, quantization, approximate_hardware,
                setup_notes, source_id, source_url, source_record
         FROM local_runtimes ORDER BY provider_id, model_id, runtime`,
      )
    ).map(mapLocalRuntime)

    const providers = (
      await readRows(
        connection,
        `SELECT provider_id, name, npm, api_url, doc_url, env_keys, retained,
                source_id, source_url, source_record
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
      canonicalModels,
      families,
      companies,
      companyPlans,
      harnesses,
      providerHarnesses,
      benchmarks,
      benchmarkScores,
      plans,
      offers,
      localRuntimes,
    }
  } finally {
    connection.closeSync()
    instance.closeSync()
  }
}

export function familyPath(catalog: CatalogData, familyId: string): CatalogFamily[] {
  const byId = new Map(catalog.families.map((family) => [family.familyId, family]))
  const path: CatalogFamily[] = []
  const seen = new Set<string>()
  let current = byId.get(familyId)
  while (current) {
    if (seen.has(current.familyId)) {
      break
    }
    seen.add(current.familyId)
    path.push(current)
    current = current.parentFamilyId ? byId.get(current.parentFamilyId) : undefined
  }
  return path
}

export function descendantFamilyIds(catalog: CatalogData, familyId: string): string[] {
  return catalog.families
    .filter((family) => familyPath(catalog, family.familyId).some((entry) => entry.familyId === familyId))
    .map((family) => family.familyId)
}

export interface ProviderCompanyContext {
  company: CatalogCompany
  plan: CatalogCompanyPlan
}

/** The curated company plan that serves this provider entry, when one exists. */
export function companyContextForProvider(
  catalog: CatalogData,
  providerId: string,
): ProviderCompanyContext | null {
  const plan = catalog.companyPlans.find((entry) => entry.providerId === providerId)
  if (!plan) {
    return null
  }
  const company = catalog.companies.find((entry) => entry.companyId === plan.companyId)
  return company ? { company, plan } : null
}

export function plansForCompany(catalog: CatalogData, companyId: string): CatalogCompanyPlan[] {
  return catalog.companyPlans.filter((plan) => plan.companyId === companyId)
}

export function harnessesForProvider(
  catalog: CatalogData,
  providerId: string,
): CatalogHarness[] {
  const harnessIds = new Set(
    catalog.providerHarnesses
      .filter((link) => link.providerId === providerId)
      .map((link) => link.harnessId),
  )
  return catalog.harnesses.filter((harness) => harnessIds.has(harness.harnessId))
}

export function benchmarkScoresFor(
  catalog: CatalogData,
  canonicalModelId: string,
): { benchmark: CatalogBenchmark; score: CatalogBenchmarkScore }[] {
  const benchmarks = new Map(catalog.benchmarks.map((benchmark) => [benchmark.benchmarkId, benchmark]))
  return catalog.benchmarkScores
    .filter((score) => score.canonicalModelId === canonicalModelId)
    .flatMap((score) => {
      const benchmark = benchmarks.get(score.benchmarkId)
      return benchmark ? [{ benchmark, score }] : []
    })
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
    retained: booleanValue(row.retained),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
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
    canonicalModelId: stringValue(row.canonical_model_id),
    canonicalModelName: stringValue(row.canonical_model_name),
    familyId: stringValue(row.family_id),
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
    retained: booleanValue(row.retained),
    sourceId: stringValue(row.source_id),
    sourceUrl: stringValue(row.source_url),
    sourceRecord: stringValue(row.source_record),
  }
}

function mapCanonicalModel(row: Record<string, DuckDBValue>): CatalogCanonicalModel {
  return {
    canonicalModelId: stringValue(row.canonical_model_id),
    name: stringValue(row.name),
    lab: stringValue(row.lab),
    familyId: stringValue(row.family_id),
  }
}

function mapCompany(row: Record<string, DuckDBValue>): CatalogCompany {
  return {
    companyId: stringValue(row.company_id),
    name: stringValue(row.name),
    homepageUrl: nullableString(row.homepage_url),
    sourceUrl: stringValue(row.source_url),
  }
}

function mapCompanyPlan(row: Record<string, DuckDBValue>): CatalogCompanyPlan {
  return {
    planId: stringValue(row.plan_id),
    companyId: stringValue(row.company_id),
    name: stringValue(row.name),
    providerId: nullableString(row.provider_id),
    sourceUrl: stringValue(row.source_url),
  }
}

function mapFamily(row: Record<string, DuckDBValue>): CatalogFamily {
  return {
    familyId: stringValue(row.family_id),
    name: stringValue(row.name),
    parentFamilyId: nullableString(row.parent_family_id),
    vendor: nullableString(row.vendor),
  }
}

function mapHarness(row: Record<string, DuckDBValue>): CatalogHarness {
  return {
    harnessId: stringValue(row.harness_id),
    name: stringValue(row.name),
    vendor: nullableString(row.vendor),
    homepageUrl: nullableString(row.homepage_url),
  }
}

function mapProviderHarness(row: Record<string, DuckDBValue>): CatalogProviderHarness {
  return {
    providerId: stringValue(row.provider_id),
    harnessId: stringValue(row.harness_id),
    exclusive: booleanValue(row.exclusive),
    sourceUrl: stringValue(row.source_url),
  }
}

function mapBenchmark(row: Record<string, DuckDBValue>): CatalogBenchmark {
  return {
    benchmarkId: stringValue(row.benchmark_id),
    name: stringValue(row.name),
    description: nullableString(row.description),
    unit: benchmarkUnit(row.unit),
    higherIsBetter: booleanValue(row.higher_is_better),
    sourceUrl: stringValue(row.source_url),
  }
}

function mapBenchmarkScore(row: Record<string, DuckDBValue>): CatalogBenchmarkScore {
  return {
    canonicalModelId: stringValue(row.canonical_model_id),
    benchmarkId: stringValue(row.benchmark_id),
    score: numberValue(row.score),
    sourceUrl: stringValue(row.source_url),
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

function booleanValue(value: DuckDBValue): boolean {
  if (typeof value === 'boolean') {
    return value
  }
  throw new Error(`Expected a boolean, got ${String(value)}`)
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

function benchmarkUnit(value: DuckDBValue): BenchmarkUnit {
  if (value === 'percent' || value === 'score') {
    return value
  }
  throw new Error(`Unknown benchmark unit: ${String(value)}`)
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
