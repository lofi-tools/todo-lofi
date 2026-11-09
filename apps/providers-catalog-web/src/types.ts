export type Confidence = 'verified' | 'community' | 'inferred' | 'unknown'
export type ScrapeStatus = 'ok' | 'missing' | 'failed' | 'not_fetched'

export interface Provenance {
  sourceId: string
  sourceUrl: string
  sourceRecord: string
  verifiedAt: string
  lastAttemptAt: string
  confidence: Confidence
  scrapeStatus: ScrapeStatus
  lastError: string | null
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
}

export interface CatalogModel extends Provenance {
  providerId: string
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
  providers: number
  models: number
  agents: number
  plans: number
  freeOffers: number
  localRuntimes: number
}
