import { createHash, randomUUID } from 'node:crypto'
import { existsSync } from 'node:fs'
import { mkdir, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { DuckDBInstance, type DuckDBConnection, type DuckDBValue } from '@duckdb/node-api'
import { enrichCatalog } from './curated'
import {
  freeOfferPolicyViolation,
  normalizeFreeOffers,
  type FreeOfferMergeReport,
} from './free-offers'
import { mergePreviousFacts, type MergeReport } from './merge'
import { loadSchemaSql, SCHEMA_VERSION } from './schema'
import type {
  CatalogBenchmark,
  CatalogBenchmarkScore,
  CatalogCanonicalModel,
  CatalogCompany,
  CatalogCompanyPlan,
  CatalogFamily,
  CatalogFreeOffer,
  CatalogHarness,
  CatalogLocalRuntime,
  CatalogModel,
  CatalogPlan,
  CatalogProvider,
  CatalogProviderHarness,
  CatalogSource,
  ModelsDevCatalog,
  SnapshotCounts,
} from './types'

const SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1000

const SOURCE_TABLES = ['providers', 'models', 'plans', 'free_offers', 'local_runtimes'] as const

export interface CreateSnapshotOptions {
  catalog: ModelsDevCatalog
  source: CatalogSource
  fetchedAt: string
  contentSha256: string
  httpStatus: number
  etag: string | null
  outputDirectory?: string
  now?: Date
  previousSnapshotPath?: string
  plans?: CatalogPlan[]
  freeOffers?: CatalogFreeOffer[]
  localRuntimes?: CatalogLocalRuntime[]
}

export interface SnapshotReport {
  snapshotId: string
  snapshotPath: string
  checksumPath: string
  checksum: string
  schemaVersion: number
  generatedAt: string
  counts: SnapshotCounts
  merge: MergeReport | null
  offerMerge: FreeOfferMergeReport | null
}

export interface ValidationReport {
  snapshotId: string
  schemaVersion: number
  generatedAt: string
  stale: boolean
  ageDays: number
  counts: SnapshotCounts
  knownGaps: string[]
  errors: string[]
  warnings: string[]
  ok: boolean
}

export interface Freshness {
  stale: boolean
  ageDays: number
  deadline: string
}

export class SnapshotValidationError extends Error {
  readonly report: ValidationReport

  constructor(report: ValidationReport) {
    super(`Snapshot validation failed: ${report.errors.join('; ')}`)
    this.name = 'SnapshotValidationError'
    this.report = report
  }
}

export async function createSnapshot(options: CreateSnapshotOptions): Promise<SnapshotReport> {
  const now = options.now ?? new Date()
  const generatedAt = now.toISOString()
  const timestamp = compactTimestamp(now)
  const snapshotId = `${timestamp}-${options.contentSha256.slice(0, 8)}`
  const outputDirectory = path.resolve(options.outputDirectory ?? path.join(process.cwd(), 'artifacts'))
  const fileName = `providers-catalog-${SCHEMA_VERSION}-${snapshotId}.duckdb`
  const snapshotPath = path.join(outputDirectory, fileName)
  const checksumPath = `${snapshotPath}.sha256`
  const tempPath = path.join(outputDirectory, `.tmp-${randomUUID()}.duckdb`)
  const merged = options.previousSnapshotPath
    ? await mergePreviousFacts(options.catalog, options.previousSnapshotPath, options.fetchedAt)
    : null
  const baseCatalog = merged?.catalog ?? options.catalog
  const enrichment = enrichCatalog(baseCatalog.providers, baseCatalog.models)
  const normalizedFreeOffers = normalizeFreeOffers(options.freeOffers ?? [])

  await mkdir(outputDirectory, { recursive: true })

  let instance: DuckDBInstance | null = null
  let connection: DuckDBConnection | null = null

  try {
    instance = await DuckDBInstance.create(tempPath)
    connection = await instance.connect()
    await connection.run(loadSchemaSql())
    await connection.run('BEGIN TRANSACTION')

    try {
      await insertSource(connection, options, snapshotId, generatedAt)
      await insertProviders(connection, baseCatalog.providers)
      await insertCompanies(connection, enrichment.companies)
      await insertCompanyPlans(connection, enrichment.companyPlans)
      await insertFamilies(connection, enrichment.families)
      await insertCanonicalModels(connection, enrichment.canonicalModels)
      await insertModels(connection, enrichment.models)
      await insertHarnesses(connection, enrichment.harnesses)
      await insertProviderHarnesses(connection, enrichment.providerHarnesses)
      await insertBenchmarks(connection, enrichment.benchmarks)
      await insertBenchmarkScores(connection, enrichment.benchmarkScores)
      await insertPlans(connection, options.plans ?? [])
      await insertFreeOffers(connection, normalizedFreeOffers.offers)
      await insertLocalRuntimes(connection, options.localRuntimes ?? [])
      await connection.run('COMMIT')
    } catch (error) {
      try {
        await connection.run('ROLLBACK')
      } catch (rollbackError) {
        throw new AggregateError(
          [error, rollbackError],
          'Snapshot transaction failed and could not roll back',
        )
      }
      throw error
    }

    connection.closeSync()
    connection = null
    instance.closeSync()
    instance = null

    const validation = await validateSnapshotFile(tempPath, now)
    if (!validation.ok) {
      throw new SnapshotValidationError(validation)
    }

    await rename(tempPath, snapshotPath)
    const checksum = await sha256File(snapshotPath)

    try {
      await writeFile(checksumPath, `${checksum}  ${fileName}\n`, 'utf8')
    } catch (error) {
      await rm(snapshotPath, { force: true })
      throw error
    }

    return {
      snapshotId,
      snapshotPath,
      checksumPath,
      checksum,
      schemaVersion: SCHEMA_VERSION,
      generatedAt,
      counts: validation.counts,
      merge: merged?.report ?? null,
      offerMerge:
        normalizedFreeOffers.report.input === 0 ? null : normalizedFreeOffers.report,
    }
  } catch (error) {
    connection?.closeSync()
    instance?.closeSync()
    await rm(tempPath, { force: true })
    await rm(`${tempPath}.wal`, { force: true })
    throw error
  }
}

export async function validateSnapshotFile(
  snapshotPath: string,
  now: Date = new Date(),
): Promise<ValidationReport> {
  const instance = await DuckDBInstance.create(snapshotPath, { access_mode: 'READ_ONLY' })
  const connection = await instance.connect()

  try {
    const metaRows = await readRows(
      connection,
      `SELECT snapshot_id, schema_version, epoch_ms(generated_at) AS generated_at_ms,
              known_gaps, source_hashes
       FROM catalog_meta`,
    )
    if (metaRows.length !== 1) {
      throw new Error(`catalog_meta must contain exactly one row, found ${metaRows.length}`)
    }

    const meta = metaRows[0]
    const snapshotId = stringValue(meta.snapshot_id)
    const schemaVersion = numberValue(meta.schema_version)
    const generatedAt = new Date(numberValue(meta.generated_at_ms)).toISOString()
    const counts = await readCounts(connection)
    const freshness = freshnessFor(generatedAt, now)
    const knownGaps = parseStringArray(meta.known_gaps)
    const errors: string[] = []
    const warnings: string[] = []

    if (schemaVersion !== SCHEMA_VERSION) {
      errors.push(`catalog_meta.schema_version is ${schemaVersion}, expected ${SCHEMA_VERSION}`)
    }
    if (counts.providers === 0) {
      errors.push('providers is empty')
    }
    if (counts.models === 0) {
      errors.push('models is empty')
    }
    if (counts.canonicalModels === 0) {
      errors.push('canonical_models is empty')
    }
    if ((await readCount(connection, 'SELECT COUNT(*) AS count FROM sources')) === 0) {
      errors.push('sources is empty')
    }
    if ((await readCount(connection, 'SELECT COUNT(*) AS count FROM scrape_runs')) === 0) {
      errors.push('scrape_runs is empty')
    }

    for (const table of SOURCE_TABLES) {
      const missing = await readCount(
        connection,
        `SELECT COUNT(*) AS count FROM ${table}
         WHERE source_id IS NULL
            OR source_url IS NULL
            OR source_record IS NULL`,
      )
      if (missing > 0) {
        errors.push(`${table} has ${missing} rows without a source record`)
      }
    }

    const duplicateModels = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM (
         SELECT provider_id, model_id FROM models
         GROUP BY provider_id, model_id HAVING COUNT(*) > 1
       ) duplicate_models`,
    )
    if (duplicateModels > 0) {
      errors.push(`models has ${duplicateModels} duplicate provider/model keys`)
    }

    const duplicateOffers = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM (
         SELECT provider_id, COALESCE(plan_id, ''), allowance, allowance_unit, reset_period
         FROM free_offers
         GROUP BY provider_id, COALESCE(plan_id, ''), allowance, allowance_unit, reset_period
         HAVING COUNT(*) > 1
       ) duplicate_offers`,
    )
    if (duplicateOffers > 0) {
      errors.push(`free_offers has ${duplicateOffers} duplicate allowance claims`)
    }

    const orphanCanonicalModels = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM models m
       LEFT JOIN canonical_models c ON c.canonical_model_id = m.canonical_model_id
       WHERE m.canonical_model_id IS NULL OR c.canonical_model_id IS NULL`,
    )
    if (orphanCanonicalModels > 0) {
      errors.push(`models has ${orphanCanonicalModels} rows without a canonical model`)
    }

    const orphanFamilyReferences = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM canonical_models c
       LEFT JOIN model_families f ON f.family_id = c.family_id
       WHERE c.family_id IS NULL OR f.family_id IS NULL`,
    )
    if (orphanFamilyReferences > 0) {
      errors.push(`canonical_models has ${orphanFamilyReferences} rows without a family`)
    }

    const familyRows = await readRows(
      connection,
      'SELECT family_id, parent_family_id FROM model_families',
    )
    errors.push(...familyTreeErrors(familyRows))

    const orphanPlans = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM plans p
       LEFT JOIN providers pr ON pr.provider_id = p.provider_id
       WHERE pr.provider_id IS NULL`,
    )
    if (orphanPlans > 0) {
      errors.push(`plans has ${orphanPlans} rows without a provider`)
    }

    const orphanOffers = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM free_offers o
       LEFT JOIN plans p ON p.plan_id = o.plan_id
       WHERE o.plan_id IS NOT NULL AND p.plan_id IS NULL`,
    )
    if (orphanOffers > 0) {
      errors.push(`free_offers has ${orphanOffers} rows without a plan`)
    }

    const orphanLocalRuntimes = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM local_runtimes r
       LEFT JOIN models m ON m.provider_id = r.provider_id AND m.model_id = r.model_id
       WHERE m.model_id IS NULL`,
    )
    if (orphanLocalRuntimes > 0) {
      errors.push(`local_runtimes has ${orphanLocalRuntimes} rows without a model`)
    }

    const invalidOffers = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM free_offers
       WHERE offer_kind <> 'recurring'
          OR allowance IS NULL
          OR allowance_unit IS NULL
          OR reset_period IS NULL
          OR source_url IS NULL`,
    )
    if (invalidOffers > 0) {
      errors.push(`free_offers has ${invalidOffers} rows that are not complete recurring claims`)
    }

    const excludedOfferIds: string[] = []
    const offerRows = await readRows(
      connection,
      `SELECT offer_id, offer_kind, allowance_unit, reset_period,
              eligibility, conditions
       FROM free_offers`,
    )
    for (const row of offerRows) {
      const violation = freeOfferPolicyViolation({
        offerId: stringValue(row.offer_id),
        offerKind: stringValue(row.offer_kind),
        allowanceUnit: stringValue(row.allowance_unit),
        resetPeriod: stringValue(row.reset_period),
        eligibility: nullableString(row.eligibility),
        conditions: nullableString(row.conditions),
      })
      if (violation) {
        excludedOfferIds.push(`${stringValue(row.offer_id)} (${violation})`)
      }
    }
    if (excludedOfferIds.length > 0) {
      errors.push(
        `free_offers has ${excludedOfferIds.length} policy-excluded claims: ${excludedOfferIds.join(', ')}`,
      )
    }

    const invalidPlans = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM plans
       WHERE usage_model IS NULL OR source_url IS NULL`,
    )
    if (invalidPlans > 0) {
      errors.push(`plans has ${invalidPlans} rows without a usage model or source`)
    }

    const orphanCompanyPlans = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM company_plans cp
       LEFT JOIN companies c ON c.company_id = cp.company_id
       WHERE c.company_id IS NULL`,
    )
    if (orphanCompanyPlans > 0) {
      errors.push(`company_plans has ${orphanCompanyPlans} plans without a company`)
    }

    const orphanPlanProviders = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM company_plans cp
       LEFT JOIN providers p ON p.provider_id = cp.provider_id
       WHERE cp.provider_id IS NOT NULL AND p.provider_id IS NULL`,
    )
    if (orphanPlanProviders > 0) {
      errors.push(`company_plans has ${orphanPlanProviders} provider entries that do not exist`)
    }

    const duplicatePlanProviders = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM (
         SELECT provider_id FROM company_plans
         WHERE provider_id IS NOT NULL
         GROUP BY provider_id HAVING COUNT(*) > 1
       ) duplicate_plan_providers`,
    )
    if (duplicatePlanProviders > 0) {
      errors.push(
        `company_plans has ${duplicatePlanProviders} provider entries linked to several plans`,
      )
    }

    const invalidCompanies = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM companies WHERE name IS NULL OR source_url IS NULL`,
    )
    if (invalidCompanies > 0) {
      errors.push(`companies has ${invalidCompanies} rows without a name or source`)
    }

    const orphanHarnessLinks = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM provider_harnesses l
       LEFT JOIN providers p ON p.provider_id = l.provider_id
       LEFT JOIN harnesses h ON h.harness_id = l.harness_id
       WHERE p.provider_id IS NULL OR h.harness_id IS NULL`,
    )
    if (orphanHarnessLinks > 0) {
      errors.push(`provider_harnesses has ${orphanHarnessLinks} links with a missing endpoint`)
    }

    const exclusiveConflicts = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM (
         SELECT provider_id FROM provider_harnesses
         WHERE exclusive
         GROUP BY provider_id HAVING COUNT(*) > 1
       ) exclusive_conflicts`,
    )
    if (exclusiveConflicts > 0) {
      errors.push(
        `provider_harnesses has ${exclusiveConflicts} exclusive providers linked to several harnesses`,
      )
    }

    const invalidBenchmarks = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM benchmarks
       WHERE unit NOT IN ('percent', 'score') OR source_url IS NULL`,
    )
    if (invalidBenchmarks > 0) {
      errors.push(`benchmarks has ${invalidBenchmarks} definitions without a known unit or source`)
    }

    const duplicateScores = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM (
         SELECT canonical_model_id, benchmark_id FROM model_benchmarks
         GROUP BY canonical_model_id, benchmark_id HAVING COUNT(*) > 1
       ) duplicate_scores`,
    )
    if (duplicateScores > 0) {
      errors.push(`model_benchmarks has ${duplicateScores} duplicate canonical/benchmark scores`)
    }

    const orphanScores = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM model_benchmarks b
       LEFT JOIN canonical_models c ON c.canonical_model_id = b.canonical_model_id
       LEFT JOIN benchmarks d ON d.benchmark_id = b.benchmark_id
       WHERE c.canonical_model_id IS NULL OR d.benchmark_id IS NULL`,
    )
    if (orphanScores > 0) {
      errors.push(`model_benchmarks has ${orphanScores} scores without a model or benchmark`)
    }

    const invalidScores = await readCount(
      connection,
      `SELECT COUNT(*) AS count FROM model_benchmarks WHERE score < 0`,
    )
    if (invalidScores > 0) {
      errors.push(`model_benchmarks has ${invalidScores} negative scores`)
    }

    if (!knownGaps) {
      errors.push('catalog_meta.known_gaps is not a JSON string array')
    }
    if (!isJson(meta.source_hashes)) {
      errors.push('catalog_meta.source_hashes is not valid JSON')
    }

    if (freshness.stale) {
      warnings.push(`Snapshot is ${freshness.ageDays.toFixed(1)} days old; values may have changed.`)
    }

    return {
      snapshotId,
      schemaVersion,
      generatedAt,
      stale: freshness.stale,
      ageDays: freshness.ageDays,
      counts,
      knownGaps: knownGaps ?? [],
      errors,
      warnings,
      ok: errors.length === 0,
    }
  } finally {
    connection.closeSync()
    instance.closeSync()
  }
}

function familyTreeErrors(rows: Record<string, DuckDBValue>[]): string[] {
  const parentOf = new Map<string, string | null>()
  for (const row of rows) {
    parentOf.set(stringValue(row.family_id), nullableString(row.parent_family_id))
  }

  const errors: string[] = []
  const missingParents = [...parentOf.entries()]
    .filter(([, parent]) => parent !== null && !parentOf.has(parent))
    .map(([familyId, parent]) => `${familyId} -> ${parent}`)
  if (missingParents.length > 0) {
    errors.push(`model_families has ${missingParents.length} missing parents: ${missingParents.join(', ')}`)
  }

  const cyclic = new Set<string>()
  for (const familyId of parentOf.keys()) {
    const seen = new Set<string>()
    let current: string | null | undefined = familyId
    while (current) {
      if (seen.has(current)) {
        cyclic.add(current)
        break
      }
      seen.add(current)
      current = parentOf.get(current)
    }
  }
  if (cyclic.size > 0) {
    errors.push(`model_families contains a cycle at ${[...cyclic].sort().join(', ')}`)
  }
  return errors
}

export function freshnessFor(generatedAt: string, now: Date = new Date()): Freshness {
  const generated = new Date(generatedAt)
  if (Number.isNaN(generated.getTime())) {
    throw new Error(`Invalid snapshot generated_at: ${generatedAt}`)
  }
  const ageMs = Math.max(0, now.getTime() - generated.getTime())
  const ageDays = ageMs / (24 * 60 * 60 * 1000)
  return {
    stale: ageDays > 7,
    ageDays,
    deadline: new Date(generated.getTime() + SEVEN_DAYS_MS).toISOString(),
  }
}

export async function readSnapshotSchemaVersion(snapshotPath: string): Promise<number> {
  const instance = await DuckDBInstance.create(snapshotPath, { access_mode: 'READ_ONLY' })
  const connection = await instance.connect()
  try {
    const rows = await readRows(connection, 'SELECT schema_version FROM catalog_meta')
    if (rows.length !== 1) {
      throw new Error(`catalog_meta must contain exactly one row, found ${rows.length}`)
    }
    return numberValue(rows[0].schema_version)
  } finally {
    connection.closeSync()
    instance.closeSync()
  }
}

export async function findLatestSnapshot(outputDirectory: string): Promise<string | null> {
  if (!existsSync(outputDirectory)) {
    return null
  }

  const entries = await readdir(outputDirectory, { withFileTypes: true })
  const candidates: { path: string; modifiedAt: number }[] = []

  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.duckdb')) {
      continue
    }
    const fullPath = path.join(outputDirectory, entry.name)
    const stats = await stat(fullPath)
    candidates.push({ path: fullPath, modifiedAt: stats.mtimeMs })
  }

  candidates.sort((left, right) => right.modifiedAt - left.modifiedAt)
  return candidates[0]?.path ?? null
}

export async function recordSnapshotFailure(options: {
  outputDirectory: string
  sourceUrl: string
  error: unknown
  occurredAt?: Date
}): Promise<string> {
  const occurredAt = options.occurredAt ?? new Date()
  const failuresDirectory = path.join(options.outputDirectory, 'failures')
  await mkdir(failuresDirectory, { recursive: true })
  const failurePath = path.join(
    failuresDirectory,
    `scrape-failure-${compactTimestamp(occurredAt)}-${randomUUID().slice(0, 8)}.json`,
  )
  const error = options.error instanceof Error ? options.error.message : String(options.error)
  await writeFile(
    failurePath,
    `${JSON.stringify({
      sourceUrl: options.sourceUrl,
      occurredAt: occurredAt.toISOString(),
      error,
    }, null, 2)}\n`,
    'utf8',
  )
  return failurePath
}

async function insertSource(
  connection: DuckDBConnection,
  options: CreateSnapshotOptions,
  snapshotId: string,
  generatedAt: string,
): Promise<void> {
  await connection.run(
    `INSERT INTO sources (
       source_id, name, source_url, license, attribution, fetched_at,
       http_status, etag, content_sha256, status, error
     ) VALUES (
       $source_id, $name, $source_url, $license, $attribution, $fetched_at,
       $http_status, $etag, $content_sha256, 'ok', NULL
     )`,
    {
      source_id: options.source.sourceId,
      name: options.source.name,
      source_url: options.source.sourceUrl,
      license: options.source.license,
      attribution: options.source.attribution,
      fetched_at: options.fetchedAt,
      http_status: options.httpStatus,
      etag: options.etag,
      content_sha256: options.contentSha256,
    },
  )

  await connection.run(
    `INSERT INTO scrape_runs (
       run_id, source_id, started_at, finished_at, status, snapshot_id, error
     ) VALUES ($run_id, $source_id, $started_at, $finished_at, 'ok', $snapshot_id, NULL)`,
    {
      run_id: `run-${snapshotId}`,
      source_id: options.source.sourceId,
      started_at: options.fetchedAt,
      finished_at: generatedAt,
      snapshot_id: snapshotId,
    },
  )

  const freshnessDeadline = new Date(
    new Date(generatedAt).getTime() + SEVEN_DAYS_MS,
  ).toISOString()

  await connection.run(
    `INSERT INTO catalog_meta (
       id, snapshot_id, schema_version, generated_at, source_fetched_at,
       freshness_deadline, license, attribution, source_urls, source_hashes, known_gaps
     ) VALUES (
       1, $snapshot_id, $schema_version, $generated_at, $source_fetched_at,
       $freshness_deadline, $license, $attribution, $source_urls, $source_hashes, $known_gaps
     )`,
    {
      snapshot_id: snapshotId,
      schema_version: SCHEMA_VERSION,
      generated_at: generatedAt,
      source_fetched_at: options.fetchedAt,
      freshness_deadline: freshnessDeadline,
      license: options.source.license,
      attribution: options.source.attribution,
      source_urls: JSON.stringify({ [options.source.sourceId]: options.source.sourceUrl }),
      source_hashes: JSON.stringify({ [options.source.sourceId]: options.contentSha256 }),
      known_gaps: JSON.stringify(options.catalog.knownGaps),
    },
  )
}

async function insertProviders(
  connection: DuckDBConnection,
  providers: CatalogProvider[],
): Promise<void> {
  const statement = `INSERT INTO providers (
    provider_id, name, npm, api_url, doc_url, env_keys, logo_path, retained,
    source_id, source_url, source_record
  ) VALUES (
    $provider_id, $name, $npm, $api_url, $doc_url, $env_keys, $logo_path, $retained,
    $source_id, $source_url, $source_record
  )`

  for (const provider of providers) {
    await connection.run(statement, {
      provider_id: provider.providerId,
      name: provider.name,
      npm: provider.npm,
      api_url: provider.apiUrl,
      doc_url: provider.docUrl,
      env_keys: JSON.stringify(provider.envKeys),
      logo_path: provider.logoPath,
      retained: provider.retained,
      source_id: provider.sourceId,
      source_url: provider.sourceUrl,
      source_record: provider.sourceRecord,
    })
  }
}

async function insertCompanies(
  connection: DuckDBConnection,
  companies: CatalogCompany[],
): Promise<void> {
  const statement = `INSERT INTO companies (
    company_id, name, homepage_url, source_url
  ) VALUES ($company_id, $name, $homepage_url, $source_url)`

  for (const company of companies) {
    await connection.run(statement, {
      company_id: company.companyId,
      name: company.name,
      homepage_url: company.homepageUrl,
      source_url: company.sourceUrl,
    })
  }
}

async function insertCompanyPlans(
  connection: DuckDBConnection,
  plans: CatalogCompanyPlan[],
): Promise<void> {
  const statement = `INSERT INTO company_plans (
    plan_id, company_id, name, provider_id, source_url
  ) VALUES ($plan_id, $company_id, $name, $provider_id, $source_url)`

  for (const plan of plans) {
    await connection.run(statement, {
      plan_id: plan.planId,
      company_id: plan.companyId,
      name: plan.name,
      provider_id: plan.providerId,
      source_url: plan.sourceUrl,
    })
  }
}

async function insertFamilies(
  connection: DuckDBConnection,
  families: CatalogFamily[],
): Promise<void> {
  const statement = `INSERT INTO model_families (
    family_id, name, parent_family_id, vendor
  ) VALUES ($family_id, $name, $parent_family_id, $vendor)`

  for (const family of families) {
    await connection.run(statement, {
      family_id: family.familyId,
      name: family.name,
      parent_family_id: family.parentFamilyId,
      vendor: family.vendor,
    })
  }
}

async function insertCanonicalModels(
  connection: DuckDBConnection,
  canonicalModels: CatalogCanonicalModel[],
): Promise<void> {
  const statement = `INSERT INTO canonical_models (
    canonical_model_id, name, lab, family_id
  ) VALUES ($canonical_model_id, $name, $lab, $family_id)`

  for (const canonicalModel of canonicalModels) {
    await connection.run(statement, {
      canonical_model_id: canonicalModel.canonicalModelId,
      name: canonicalModel.name,
      lab: canonicalModel.lab,
      family_id: canonicalModel.familyId,
    })
  }
}

async function insertModels(
  connection: DuckDBConnection,
  models: CatalogModel[],
): Promise<void> {
  const statement = `INSERT INTO models (
    provider_id, model_id, model_type, canonical_model_id, name, family, description,
    release_date, last_updated, knowledge_date, open_weights, attachment, reasoning,
    tool_call, structured_output, temperature, interleaved, context_limit, input_limit,
    output_limit, cost_input, cost_output, cost_reasoning, cost_cache_read, cost_cache_write,
    cost_input_audio, cost_output_audio, modalities_input, modalities_output, status,
    retained, source_id, source_url, source_record
  ) VALUES (
    $provider_id, $model_id, $model_type, $canonical_model_id, $name, $family, $description,
    $release_date, $last_updated, $knowledge_date, $open_weights, $attachment, $reasoning,
    $tool_call, $structured_output, $temperature, $interleaved, $context_limit, $input_limit,
    $output_limit, $cost_input, $cost_output, $cost_reasoning, $cost_cache_read, $cost_cache_write,
    $cost_input_audio, $cost_output_audio, $modalities_input, $modalities_output, $status,
    $retained, $source_id, $source_url, $source_record
  )`

  for (const model of models) {
    await connection.run(statement, {
      provider_id: model.providerId,
      model_id: model.modelId,
      model_type: model.modelType,
      canonical_model_id: model.canonicalModelId,
      name: model.name,
      family: model.family,
      description: model.description,
      release_date: model.releaseDate,
      last_updated: model.lastUpdated,
      knowledge_date: model.knowledgeDate,
      open_weights: model.openWeights,
      attachment: model.attachment,
      reasoning: model.reasoning,
      tool_call: model.toolCall,
      structured_output: model.structuredOutput,
      temperature: model.temperature,
      interleaved: model.interleaved,
      context_limit: model.contextLimit,
      input_limit: model.inputLimit,
      output_limit: model.outputLimit,
      cost_input: model.costInput,
      cost_output: model.costOutput,
      cost_reasoning: model.costReasoning,
      cost_cache_read: model.costCacheRead,
      cost_cache_write: model.costCacheWrite,
      cost_input_audio: model.costInputAudio,
      cost_output_audio: model.costOutputAudio,
      modalities_input: model.modalitiesInput === null ? null : JSON.stringify(model.modalitiesInput),
      modalities_output: model.modalitiesOutput === null ? null : JSON.stringify(model.modalitiesOutput),
      status: model.status,
      retained: model.retained,
      source_id: model.sourceId,
      source_url: model.sourceUrl,
      source_record: model.sourceRecord,
    })
  }
}

async function insertHarnesses(
  connection: DuckDBConnection,
  harnesses: CatalogHarness[],
): Promise<void> {
  const statement = `INSERT INTO harnesses (
    harness_id, name, vendor, homepage_url
  ) VALUES ($harness_id, $name, $vendor, $homepage_url)`

  for (const harness of harnesses) {
    await connection.run(statement, {
      harness_id: harness.harnessId,
      name: harness.name,
      vendor: harness.vendor,
      homepage_url: harness.homepageUrl,
    })
  }
}

async function insertProviderHarnesses(
  connection: DuckDBConnection,
  links: CatalogProviderHarness[],
): Promise<void> {
  const statement = `INSERT INTO provider_harnesses (
    provider_id, harness_id, exclusive, source_url
  ) VALUES ($provider_id, $harness_id, $exclusive, $source_url)`

  for (const link of links) {
    await connection.run(statement, {
      provider_id: link.providerId,
      harness_id: link.harnessId,
      exclusive: link.exclusive,
      source_url: link.sourceUrl,
    })
  }
}

async function insertBenchmarks(
  connection: DuckDBConnection,
  benchmarks: CatalogBenchmark[],
): Promise<void> {
  const statement = `INSERT INTO benchmarks (
    benchmark_id, name, description, unit, higher_is_better, source_url
  ) VALUES ($benchmark_id, $name, $description, $unit, $higher_is_better, $source_url)`

  for (const benchmark of benchmarks) {
    await connection.run(statement, {
      benchmark_id: benchmark.benchmarkId,
      name: benchmark.name,
      description: benchmark.description,
      unit: benchmark.unit,
      higher_is_better: benchmark.higherIsBetter,
      source_url: benchmark.sourceUrl,
    })
  }
}

async function insertBenchmarkScores(
  connection: DuckDBConnection,
  scores: CatalogBenchmarkScore[],
): Promise<void> {
  const statement = `INSERT INTO model_benchmarks (
    canonical_model_id, benchmark_id, score, source_url
  ) VALUES ($canonical_model_id, $benchmark_id, $score, $source_url)`

  for (const score of scores) {
    await connection.run(statement, {
      canonical_model_id: score.canonicalModelId,
      benchmark_id: score.benchmarkId,
      score: score.score,
      source_url: score.sourceUrl,
    })
  }
}

async function insertPlans(
  connection: DuckDBConnection,
  plans: CatalogPlan[],
): Promise<void> {
  const statement = `INSERT INTO plans (
    plan_id, provider_id, name, usage_model, billing_period, price, currency,
    included_allowance, allowance_unit, source_id, source_url, source_record
  ) VALUES (
    $plan_id, $provider_id, $name, $usage_model, $billing_period, $price, $currency,
    $included_allowance, $allowance_unit, $source_id, $source_url, $source_record
  )`

  for (const plan of plans) {
    await connection.run(statement, {
      plan_id: plan.planId,
      provider_id: plan.providerId,
      name: plan.name,
      usage_model: plan.usageModel,
      billing_period: plan.billingPeriod,
      price: plan.price,
      currency: plan.currency,
      included_allowance: plan.includedAllowance,
      allowance_unit: plan.allowanceUnit,
      source_id: plan.sourceId,
      source_url: plan.sourceUrl,
      source_record: plan.sourceRecord,
    })
  }
}

async function insertLocalRuntimes(
  connection: DuckDBConnection,
  runtimes: CatalogLocalRuntime[],
): Promise<void> {
  const statement = `INSERT INTO local_runtimes (
    provider_id, model_id, runtime, quantization, approximate_hardware, setup_notes,
    source_id, source_url, source_record
  ) VALUES (
    $provider_id, $model_id, $runtime, $quantization, $approximate_hardware, $setup_notes,
    $source_id, $source_url, $source_record
  )`

  for (const runtime of runtimes) {
    await connection.run(statement, {
      provider_id: runtime.providerId,
      model_id: runtime.modelId,
      runtime: runtime.runtime,
      quantization: runtime.quantization,
      approximate_hardware: runtime.approximateHardware,
      setup_notes: runtime.setupNotes,
      source_id: runtime.sourceId,
      source_url: runtime.sourceUrl,
      source_record: runtime.sourceRecord,
    })
  }
}

async function insertFreeOffers(
  connection: DuckDBConnection,
  offers: CatalogFreeOffer[],
): Promise<void> {
  const statement = `INSERT INTO free_offers (
    offer_id, provider_id, plan_id, offer_kind, allowance, allowance_unit,
    reset_period, eligibility, conditions, source_id, source_url, source_record
  ) VALUES (
    $offer_id, $provider_id, $plan_id, $offer_kind, $allowance, $allowance_unit,
    $reset_period, $eligibility, $conditions, $source_id, $source_url, $source_record
  )`

  for (const offer of offers) {
    await connection.run(statement, {
      offer_id: offer.offerId,
      provider_id: offer.providerId,
      plan_id: offer.planId,
      offer_kind: offer.offerKind,
      allowance: offer.allowance,
      allowance_unit: offer.allowanceUnit,
      reset_period: offer.resetPeriod,
      eligibility: offer.eligibility,
      conditions: offer.conditions,
      source_id: offer.sourceId,
      source_url: offer.sourceUrl,
      source_record: offer.sourceRecord,
    })
  }
}

async function readRows(
  connection: DuckDBConnection,
  sql: string,
): Promise<Record<string, DuckDBValue>[]> {
  const reader = await connection.runAndReadAll(sql)
  return reader.getRowObjects()
}

async function readCount(connection: DuckDBConnection, sql: string): Promise<number> {
  const rows = await readRows(connection, sql)
  const value = rows[0]?.count
  if (value === undefined) {
    throw new Error(`Expected a count column from: ${sql}`)
  }
  return numberValue(value)
}

async function readCounts(connection: DuckDBConnection): Promise<SnapshotCounts> {
  const rows = await readRows(
    connection,
    `SELECT
       (SELECT COUNT(*) FROM companies) AS companies,
       (SELECT COUNT(*) FROM company_plans) AS company_plans,
       (SELECT COUNT(*) FROM providers) AS providers,
       (SELECT COUNT(*) FROM models) AS models,
       (SELECT COUNT(*) FROM canonical_models) AS canonical_models,
       (SELECT COUNT(*) FROM model_families) AS families,
       (SELECT COUNT(*) FROM harnesses) AS harnesses,
       (SELECT COUNT(*) FROM benchmarks) AS benchmarks,
       (SELECT COUNT(*) FROM plans) AS plans,
       (SELECT COUNT(*) FROM free_offers) AS free_offers,
       (SELECT COUNT(*) FROM local_runtimes) AS local_runtimes`,
  )
  const row = rows[0]
  if (!row) {
    throw new Error('Could not read snapshot counts')
  }
  return {
    companies: numberValue(row.companies),
    companyPlans: numberValue(row.company_plans),
    providers: numberValue(row.providers),
    models: numberValue(row.models),
    canonicalModels: numberValue(row.canonical_models),
    families: numberValue(row.families),
    harnesses: numberValue(row.harnesses),
    benchmarks: numberValue(row.benchmarks),
    plans: numberValue(row.plans),
    freeOffers: numberValue(row.free_offers),
    localRuntimes: numberValue(row.local_runtimes),
  }
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
  throw new Error(`Expected a numeric value, got ${String(value)}`)
}

function stringValue(value: DuckDBValue): string {
  if (typeof value === 'string') {
    return value
  }
  throw new Error(`Expected a string value, got ${String(value)}`)
}

function nullableString(value: DuckDBValue): string | null {
  return typeof value === 'string' ? value : null
}

function isJson(value: DuckDBValue): boolean {
  if (typeof value !== 'string') {
    return false
  }
  try {
    JSON.parse(value)
    return true
  } catch {
    return false
  }
}

function parseStringArray(value: DuckDBValue): string[] | null {
  if (typeof value !== 'string') {
    return null
  }
  try {
    const parsed: unknown = JSON.parse(value)
    if (!Array.isArray(parsed) || !parsed.every((item) => typeof item === 'string')) {
      return null
    }
    return parsed
  } catch {
    return null
  }
}

async function sha256File(filePath: string): Promise<string> {
  return createHash('sha256').update(await readFile(filePath)).digest('hex')
}

function compactTimestamp(date: Date): string {
  return date.toISOString().replace(/[-:]/g, '').replace(/\.\d{3}Z$/, 'Z')
}
