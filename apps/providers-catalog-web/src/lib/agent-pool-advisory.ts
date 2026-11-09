import type { CatalogData } from './catalog'

export const AGENT_POOL_ADVISORY_VERSION = 1

export interface AgentPoolAdvisoryRecord {
  providerId: string
  modelId: string | null
  planId: string | null
  offerId: string
  allowance: number
  allowanceUnit: string
  resetPeriod: string
  usageModel: string
  capacity: 'known' | 'unknown'
  confidence: string
  verifiedAt: string
  scrapeStatus: string
  snapshotId: string
  schemaVersion: number
  stale: boolean
  sourceUrl: string
  conditions: string | null
}

export interface AgentPoolAdvisory {
  contractVersion: number
  snapshotId: string
  schemaVersion: number
  generatedAt: string
  stale: boolean
  records: AgentPoolAdvisoryRecord[]
}

export interface LocalAllowanceObservation {
  source: 'local_usage' | 'provider_response'
  allowance: number
  allowanceUnit: string
  resetPeriod: string
  observedAt: string
}

export function buildAgentPoolAdvisory(catalog: CatalogData): AgentPoolAdvisory {
  const plansById = new Map(catalog.plans.map((plan) => [plan.planId, plan]))
  const records = catalog.offers.map((offer) => {
    const plan = offer.planId ? plansById.get(offer.planId) : undefined
    const usageModel = plan?.usageModel ?? 'unknown'
    const record: AgentPoolAdvisoryRecord = {
      providerId: offer.providerId,
      modelId: null,
      planId: offer.planId,
      offerId: offer.offerId,
      allowance: offer.allowance,
      allowanceUnit: offer.allowanceUnit,
      resetPeriod: offer.resetPeriod,
      usageModel,
      capacity: 'unknown',
      confidence: offer.confidence,
      verifiedAt: offer.verifiedAt,
      scrapeStatus: offer.scrapeStatus,
      snapshotId: catalog.snapshot.snapshotId,
      schemaVersion: catalog.snapshot.schemaVersion,
      stale: catalog.snapshot.stale,
      sourceUrl: offer.sourceUrl,
      conditions: offer.conditions,
    }
    return {
      ...record,
      capacity: advisoryCapacity(record),
    }
  })

  return {
    contractVersion: AGENT_POOL_ADVISORY_VERSION,
    snapshotId: catalog.snapshot.snapshotId,
    schemaVersion: catalog.snapshot.schemaVersion,
    generatedAt: catalog.snapshot.generatedAt,
    stale: catalog.snapshot.stale,
    records,
  }
}

export function advisoryCapacity(
  record: Pick<AgentPoolAdvisoryRecord, 'stale' | 'scrapeStatus' | 'allowance' | 'usageModel'>,
): 'known' | 'unknown' {
  if (
    record.stale ||
    record.scrapeStatus !== 'ok' ||
    record.allowance === null ||
    record.usageModel === 'unknown'
  ) {
    return 'unknown'
  }
  return 'known'
}

export function applyLocalAllowance(
  record: AgentPoolAdvisoryRecord,
  observation: LocalAllowanceObservation,
): AgentPoolAdvisoryRecord {
  return {
    ...record,
    allowance: observation.allowance,
    allowanceUnit: observation.allowanceUnit,
    resetPeriod: observation.resetPeriod,
    capacity: 'known',
    confidence: 'verified',
    verifiedAt: observation.observedAt,
  }
}
