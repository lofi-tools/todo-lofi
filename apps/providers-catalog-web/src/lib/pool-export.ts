import type { CatalogData } from './catalog'
import {
  AGENT_POOL_ADVISORY_VERSION,
  advisoryCapacity,
  type AgentPoolAdvisoryRecord,
} from './agent-pool-advisory'

/**
 * The catalog covers more harnesses than Shepherd can run. The pool export is
 * limited to the harnesses Shepherd actually implements; Pi has no catalog
 * harness yet, so it stays out of the generated data.
 */
export const SHEPHERD_HARNESSES = [
  { harnessId: 'claude-code', agent: 'claude' },
  { harnessId: 'codex', agent: 'codex' },
  { harnessId: 'opencode', agent: 'opencode' },
] as const

export interface PoolModelChoice {
  modelId: string
  name: string
  contextLimit: number | null
}

export interface PoolProviderChoice {
  providerId: string
  providerName: string
  models: PoolModelChoice[]
}

export interface PoolHarnessChoice {
  harnessId: string
  agent: string
  name: string
  providers: PoolProviderChoice[]
}

export interface ShepherdPoolCatalog {
  contractVersion: number
  snapshotId: string
  schemaVersion: number
  generatedAt: string
  stale: boolean
  records: AgentPoolAdvisoryRecord[]
  harnesses: PoolHarnessChoice[]
}

export function buildShepherdPoolCatalog(catalog: CatalogData): ShepherdPoolCatalog {
  const providerNames = new Map(
    catalog.providers.map((provider) => [provider.providerId, provider.name]),
  )
  const plansById = new Map(catalog.plans.map((plan) => [plan.planId, plan]))
  const offersByProvider = new Map<string, typeof catalog.offers>()
  for (const offer of catalog.offers) {
    const offers = offersByProvider.get(offer.providerId) ?? []
    offers.push(offer)
    offersByProvider.set(offer.providerId, offers)
  }

  const harnesses: PoolHarnessChoice[] = SHEPHERD_HARNESSES.map(({ harnessId, agent }) => {
    const harness = catalog.harnesses.find((candidate) => candidate.harnessId === harnessId)
    const providerIds = [
      ...new Set(
        catalog.providerHarnesses
          .filter((link) => link.harnessId === harnessId)
          .map((link) => link.providerId),
      ),
    ].sort()

    const providers = providerIds
      .map((providerId): PoolProviderChoice => {
        const models = catalog.models
          .filter((model) => model.providerId === providerId)
          .map((model) => ({
            modelId: model.modelId,
            name: model.name ?? model.modelId,
            contextLimit: model.contextLimit,
          }))
          .sort((left, right) => left.modelId.localeCompare(right.modelId))
        return {
          providerId,
          providerName: providerNames.get(providerId) ?? providerId,
          models,
        }
      })
      .filter((provider) => provider.models.length > 0)

    return {
      harnessId,
      agent,
      name: harness?.name ?? harnessId,
      providers,
    }
  })

  const records: AgentPoolAdvisoryRecord[] = []
  for (const harness of harnesses) {
    for (const provider of harness.providers) {
      const offers = offersByProvider.get(provider.providerId) ?? []
      // More than one recurring offer makes the provider's allowance
      // ambiguous for a model; the pool keeps that capacity unknown instead of
      // picking one.
      if (offers.length !== 1) continue
      const offer = offers[0]
      const plan = offer.planId ? plansById.get(offer.planId) : undefined
      const base = {
        providerId: provider.providerId,
        planId: offer.planId,
        offerId: offer.offerId,
        allowance: offer.allowance,
        allowanceUnit: offer.allowanceUnit,
        resetPeriod: offer.resetPeriod,
        usageModel: plan?.usageModel ?? 'unknown',
        snapshotId: catalog.snapshot.snapshotId,
        schemaVersion: catalog.snapshot.schemaVersion,
        stale: catalog.snapshot.stale,
        sourceUrl: offer.sourceUrl,
        conditions: offer.conditions,
      }
      for (const model of provider.models) {
        const record: AgentPoolAdvisoryRecord = {
          ...base,
          modelId: model.modelId,
          capacity: 'unknown',
        }
        records.push({ ...record, capacity: advisoryCapacity(record) })
      }
    }
  }

  records.sort(
    (left, right) =>
      left.providerId.localeCompare(right.providerId) ||
      (left.modelId ?? '').localeCompare(right.modelId ?? ''),
  )

  return {
    contractVersion: AGENT_POOL_ADVISORY_VERSION,
    snapshotId: catalog.snapshot.snapshotId,
    schemaVersion: catalog.snapshot.schemaVersion,
    generatedAt: catalog.snapshot.generatedAt,
    stale: catalog.snapshot.stale,
    records,
    harnesses,
  }
}
