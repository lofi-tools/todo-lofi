export interface Provenance {
  sourceId: string
  sourceUrl: string
  sourceRecord: string
}

export interface CatalogSource {
  sourceId: string
  name: string
  sourceUrl: string
  license: string
  attribution: string
}

export interface CatalogProvider extends Provenance {
  providerId: string
  name: string
  npm: string | null
  apiUrl: string | null
  docUrl: string | null
  envKeys: string[]
  logoPath: string | null
  retained: boolean
}

export interface CatalogCompany {
  companyId: string
  name: string
  homepageUrl: string | null
  sourceUrl: string
}

export interface CatalogCompanyPlan {
  planId: string
  companyId: string
  name: string
  providerId: string | null
  sourceUrl: string
}

export interface CatalogFamily {
  familyId: string
  name: string
  parentFamilyId: string | null
  vendor: string | null
}

export interface CatalogCanonicalModel {
  canonicalModelId: string
  name: string
  lab: string
  familyId: string
}

export interface CatalogModel extends Provenance {
  providerId: string
  modelId: string
  modelType: string | null
  canonicalModelId: string
  name: string | null
  family: string | null
  familyId: string
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
}

export interface CatalogHarness {
  harnessId: string
  name: string
  vendor: string | null
  homepageUrl: string | null
}

export interface CatalogProviderHarness {
  providerId: string
  harnessId: string
  exclusive: boolean
  sourceUrl: string
}

export type BenchmarkUnit = 'percent' | 'score'

export interface CatalogBenchmark {
  benchmarkId: string
  name: string
  description: string | null
  unit: BenchmarkUnit
  higherIsBetter: boolean
  sourceUrl: string
}

export interface CatalogBenchmarkScore {
  canonicalModelId: string
  benchmarkId: string
  score: number
  sourceUrl: string
}

export type PlanUsageModel =
  | 'subscription'
  | 'token_pack'
  | 'pay_as_you_go'
  | 'free'
  | 'unknown'

export interface CatalogPlan extends Provenance {
  planId: string
  providerId: string
  name: string
  usageModel: PlanUsageModel
  billingPeriod: string | null
  price: number | null
  currency: string | null
  includedAllowance: number | null
  allowanceUnit: string | null
}

export interface CatalogLocalRuntime extends Provenance {
  providerId: string
  modelId: string
  runtime: string
  quantization: string | null
  approximateHardware: string | null
  setupNotes: string | null
}

export interface CatalogFreeOffer extends Provenance {
  offerId: string
  providerId: string
  planId: string | null
  offerKind: 'recurring'
  allowance: number
  allowanceUnit: string
  resetPeriod: string
  eligibility: string | null
  conditions: string | null
}

export interface ModelsDevCatalog {
  providers: CatalogProvider[]
  models: CatalogModel[]
  knownGaps: string[]
}

export interface ModelsDevFetchResult {
  catalog: ModelsDevCatalog
  sourceUrl: string
  fetchedAt: string
  contentSha256: string
  httpStatus: number
  etag: string | null
}

export interface SnapshotCounts {
  companies: number
  companyPlans: number
  providers: number
  models: number
  canonicalModels: number
  families: number
  harnesses: number
  benchmarks: number
  plans: number
  freeOffers: number
  localRuntimes: number
}
