import type {
  CatalogData,
  CatalogLocalRuntime,
  CatalogModel,
  CatalogOffer,
  CatalogPlan,
} from './catalog'
import { modelHref, modelKey } from './format'

export interface ComparisonPlan {
  name: string
  usageModel: string
  sourceName: string
  sourceUrl: string
}

export interface ComparisonOffer {
  allowance: number
  allowanceUnit: string
  resetPeriod: string
  conditions: string | null
  sourceName: string
  sourceUrl: string
}

export interface ComparisonRuntime {
  runtime: string
  quantization: string | null
  approximateHardware: string | null
  sourceName: string
  sourceUrl: string
}

export interface ComparisonModel {
  key: string
  href: string
  providerId: string
  providerName: string
  modelId: string
  name: string | null
  modelType: string | null
  canonicalModelId: string | null
  contextLimit: number | null
  inputLimit: number | null
  outputLimit: number | null
  modalitiesInput: string[] | null
  modalitiesOutput: string[] | null
  openWeights: boolean | null
  costInput: number | null
  costOutput: number | null
  costCacheRead: number | null
  costCacheWrite: number | null
  sourceName: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  confidence: string
  plans: ComparisonPlan[]
  offers: ComparisonOffer[]
  localRuntimes: ComparisonRuntime[]
}

export function buildComparisonModels(catalog: CatalogData): ComparisonModel[] {
  const sources = new Map(
    catalog.snapshot.sources.map((source) => [
      source.sourceId,
      { sourceName: source.name, sourceUrl: source.sourceUrl },
    ]),
  )
  const sourceFor = (sourceId: string, fallbackUrl: string) =>
    sources.get(sourceId) ?? { sourceName: fallbackUrl, sourceUrl: fallbackUrl }

  return catalog.models.map((model) => ({
    key: modelKey(model.providerId, model.modelId),
    href: modelHref(model.providerId, model.modelId),
    providerId: model.providerId,
    providerName: model.providerName,
    modelId: model.modelId,
    name: model.name,
    modelType: model.modelType,
    canonicalModelId: model.canonicalModelId,
    contextLimit: model.contextLimit,
    inputLimit: model.inputLimit,
    outputLimit: model.outputLimit,
    modalitiesInput: model.modalitiesInput,
    modalitiesOutput: model.modalitiesOutput,
    openWeights: model.openWeights,
    costInput: model.costInput,
    costOutput: model.costOutput,
    costCacheRead: model.costCacheRead,
    costCacheWrite: model.costCacheWrite,
    sourceRecord: model.sourceRecord,
    verifiedAt: model.verifiedAt,
    confidence: model.confidence,
    ...sourceFor(model.sourceId, model.sourceUrl),
    plans: catalog.plans
      .filter((plan) => plan.providerId === model.providerId)
      .map((plan) => planSummary(plan, sourceFor)),
    offers: catalog.offers
      .filter((offer) => offer.providerId === model.providerId)
      .map((offer) => offerSummary(offer, sourceFor)),
    localRuntimes: catalog.localRuntimes
      .filter(
        (runtime) =>
          runtime.providerId === model.providerId && runtime.modelId === model.modelId,
      )
      .map((runtime) => runtimeSummary(runtime, sourceFor)),
  }))
}

function planSummary(
  plan: CatalogPlan,
  sourceFor: (sourceId: string, fallbackUrl: string) => { sourceName: string; sourceUrl: string },
): ComparisonPlan {
  return {
    name: plan.name,
    usageModel: plan.usageModel,
    ...sourceFor(plan.sourceId, plan.sourceUrl),
  }
}

function offerSummary(
  offer: CatalogOffer,
  sourceFor: (sourceId: string, fallbackUrl: string) => { sourceName: string; sourceUrl: string },
): ComparisonOffer {
  return {
    allowance: offer.allowance,
    allowanceUnit: offer.allowanceUnit,
    resetPeriod: offer.resetPeriod,
    conditions: offer.conditions,
    ...sourceFor(offer.sourceId, offer.sourceUrl),
  }
}

function runtimeSummary(
  runtime: CatalogLocalRuntime,
  sourceFor: (sourceId: string, fallbackUrl: string) => { sourceName: string; sourceUrl: string },
): ComparisonRuntime {
  return {
    runtime: runtime.runtime,
    quantization: runtime.quantization,
    approximateHardware: runtime.approximateHardware,
    ...sourceFor(runtime.sourceId, runtime.sourceUrl),
  }
}
