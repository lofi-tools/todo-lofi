import type {
  CatalogBenchmark,
  CatalogBenchmarkScore,
  CatalogCanonicalModel,
  CatalogCompany,
  CatalogCompanyPlan,
  CatalogFamily,
  CatalogHarness,
  CatalogModel,
  CatalogProvider,
  CatalogProviderHarness,
} from './types'

/**
 * Curated identity data. models.dev carries model facts and most canonical
 * model ids, but it does not describe harness compatibility, a family tree or
 * benchmark definitions. Those are reviewed here instead of being guessed from
 * wire ids at build time.
 */

const LAB_ALIASES: Record<string, string> = {
  'deepseek-ai': 'deepseek',
  'meta-llama': 'meta',
  mistralai: 'mistral',
  moonshotai: 'moonshot',
  'x-ai': 'xai',
  'z-ai': 'zai',
  zhipuai: 'zai',
  'google-vertex': 'google',
  'google-vertex-anthropic': 'anthropic',
  'qwen-ai': 'qwen',
}

const PROVIDER_LABS: Record<string, string> = {
  anthropic: 'anthropic',
  openai: 'openai',
  azure: 'openai',
  google: 'google',
  'google-vertex': 'google',
  'google-vertex-anthropic': 'anthropic',
  deepseek: 'deepseek',
  meta: 'meta',
  mistral: 'mistral',
  moonshotai: 'moonshot',
  'moonshotai-cn': 'moonshot',
  xai: 'xai',
  zai: 'zai',
  qwen: 'qwen',
  alibaba: 'qwen',
  'alibaba-cn': 'qwen',
}

const LAB_NAMES: Record<string, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI',
  google: 'Google',
  deepseek: 'DeepSeek',
  meta: 'Meta',
  mistral: 'Mistral',
  moonshot: 'Moonshot',
  xai: 'xAI',
  zai: 'Z.ai',
  qwen: 'Qwen',
  nvidia: 'NVIDIA',
  cohere: 'Cohere',
  ai21: 'AI21',
  minimax: 'MiniMax',
  stepfun: 'StepFun',
  baidu: 'Baidu',
  tencent: 'Tencent',
  xiaomi: 'Xiaomi',
  inclusionai: 'InclusionAI',
}

/**
 * The family tree is defined by `parentFamilyId` alone. A family id is also its
 * marker: the longest family id contained in a canonical model slug wins, so
 * `claude-opus-5-5` resolves to `opus-5-5`, whose parent chain supplies `opus`
 * and `claude`.
 */
export const FAMILY_TREE: CatalogFamily[] = [
  { familyId: 'claude', name: 'Claude', parentFamilyId: null, vendor: 'anthropic' },
  { familyId: 'opus', name: 'Opus', parentFamilyId: 'claude', vendor: 'anthropic' },
  { familyId: 'opus-5', name: 'Opus 5', parentFamilyId: 'opus', vendor: 'anthropic' },
  { familyId: 'opus-5-5', name: 'Opus 5.5', parentFamilyId: 'opus-5', vendor: 'anthropic' },
  { familyId: 'sonnet', name: 'Sonnet', parentFamilyId: 'claude', vendor: 'anthropic' },
  { familyId: 'sonnet-5', name: 'Sonnet 5', parentFamilyId: 'sonnet', vendor: 'anthropic' },
  { familyId: 'sonnet-5-5', name: 'Sonnet 5.5', parentFamilyId: 'sonnet-5', vendor: 'anthropic' },
  { familyId: 'haiku', name: 'Haiku', parentFamilyId: 'claude', vendor: 'anthropic' },
  { familyId: 'haiku-5', name: 'Haiku 5', parentFamilyId: 'haiku', vendor: 'anthropic' },
  { familyId: 'fable', name: 'Fable', parentFamilyId: 'claude', vendor: 'anthropic' },
  { familyId: 'deepseek', name: 'DeepSeek', parentFamilyId: null, vendor: 'deepseek' },
  { familyId: 'deepseek-v4', name: 'DeepSeek V4', parentFamilyId: 'deepseek', vendor: 'deepseek' },
  { familyId: 'deepseek-v4-1', name: 'DeepSeek V4.1', parentFamilyId: 'deepseek-v4', vendor: 'deepseek' },
  {
    familyId: 'deepseek-v4-1-0731',
    name: 'DeepSeek V4.1 0731',
    parentFamilyId: 'deepseek-v4-1',
    vendor: 'deepseek',
  },
  { familyId: 'gpt', name: 'GPT', parentFamilyId: null, vendor: 'openai' },
  { familyId: 'gpt-oss', name: 'gpt-oss', parentFamilyId: 'gpt', vendor: 'openai' },
  { familyId: 'gpt-5', name: 'GPT-5', parentFamilyId: 'gpt', vendor: 'openai' },
  { familyId: 'gpt-5-6', name: 'GPT-5.6', parentFamilyId: 'gpt-5', vendor: 'openai' },
  { familyId: 'gemini', name: 'Gemini', parentFamilyId: null, vendor: 'google' },
  { familyId: 'gemini-3', name: 'Gemini 3', parentFamilyId: 'gemini', vendor: 'google' },
  { familyId: 'qwen', name: 'Qwen', parentFamilyId: null, vendor: 'qwen' },
  { familyId: 'qwen-3', name: 'Qwen3', parentFamilyId: 'qwen', vendor: 'qwen' },
  { familyId: 'llama', name: 'Llama', parentFamilyId: null, vendor: 'meta' },
  { familyId: 'mistral', name: 'Mistral', parentFamilyId: null, vendor: 'mistral' },
  { familyId: 'grok', name: 'Grok', parentFamilyId: null, vendor: 'xai' },
  { familyId: 'kimi', name: 'Kimi', parentFamilyId: null, vendor: 'moonshot' },
  { familyId: 'glm', name: 'GLM', parentFamilyId: null, vendor: 'zai' },
  { familyId: 'minimax', name: 'MiniMax', parentFamilyId: null, vendor: 'minimax' },
]

/**
 * A provider company is the vendor; a provider entry can be one of its plans.
 * `opencode` is both a company and a harness name, so the plan name (OpenCode
 * Zen) is what disambiguates the provider entry from the harness.
 */
export const COMPANIES: CatalogCompany[] = [
  {
    companyId: 'opencode',
    name: 'OpenCode',
    homepageUrl: 'https://opencode.ai',
    sourceUrl: 'https://opencode.ai/v2/docs/console/models/',
  },
]

export const COMPANY_PLANS: CatalogCompanyPlan[] = [
  {
    planId: 'opencode-zen',
    companyId: 'opencode',
    name: 'OpenCode Zen',
    providerId: 'opencode',
    sourceUrl: 'https://opencode.ai/v2/docs/console/models/',
  },
  {
    planId: 'opencode-go',
    companyId: 'opencode',
    name: 'OpenCode Go',
    providerId: 'opencode-go',
    sourceUrl: 'https://opencode.ai/v2/docs/console/go/',
  },
  {
    planId: 'opencode-go-plus',
    companyId: 'opencode',
    name: 'OpenCode Go Plus',
    providerId: null,
    sourceUrl: 'https://opencode.ai/v2/docs/console/go/',
  },
]

export const HARNESSES: CatalogHarness[] = [
  {
    harnessId: 'claude-code',
    name: 'Claude Code',
    vendor: 'Anthropic',
    homepageUrl: 'https://www.anthropic.com/claude-code',
  },
  {
    harnessId: 'codex',
    name: 'Codex',
    vendor: 'OpenAI',
    homepageUrl: 'https://openai.com/codex',
  },
  {
    harnessId: 'gemini-cli',
    name: 'Gemini CLI',
    vendor: 'Google',
    homepageUrl: 'https://github.com/google-gemini/gemini-cli',
  },
  {
    harnessId: 'opencode',
    name: 'OpenCode',
    vendor: 'OpenCode',
    homepageUrl: 'https://opencode.ai',
  },
  {
    harnessId: 'copilot',
    name: 'GitHub Copilot',
    vendor: 'GitHub',
    homepageUrl: 'https://github.com/features/copilot',
  },
]

/**
 * Harness compatibility. An exclusive link is exclusive both ways: the provider
 * serves only that harness, and no other harness can use that provider.
 * Anthropic-served models are Claude Code only; OpenAI/GPT-served models are
 * Codex only, which is the compatibility the catalog is asked to show.
 */
export const PROVIDER_HARNESS_LINKS: CatalogProviderHarness[] = [
  {
    providerId: 'anthropic',
    harnessId: 'claude-code',
    exclusive: true,
    sourceUrl: 'https://www.anthropic.com/claude-code',
  },
  {
    providerId: 'google-vertex-anthropic',
    harnessId: 'claude-code',
    exclusive: true,
    sourceUrl: 'https://www.anthropic.com/claude-code',
  },
  {
    providerId: 'openai',
    harnessId: 'codex',
    exclusive: true,
    sourceUrl: 'https://openai.com/codex',
  },
  {
    providerId: 'azure',
    harnessId: 'codex',
    exclusive: true,
    sourceUrl: 'https://openai.com/codex',
  },
  {
    providerId: 'google',
    harnessId: 'gemini-cli',
    exclusive: true,
    sourceUrl: 'https://github.com/google-gemini/gemini-cli',
  },
  {
    providerId: 'google-vertex',
    harnessId: 'gemini-cli',
    exclusive: true,
    sourceUrl: 'https://github.com/google-gemini/gemini-cli',
  },
  {
    providerId: 'opencode',
    harnessId: 'opencode',
    exclusive: false,
    sourceUrl: 'https://opencode.ai/docs/providers',
  },
  {
    providerId: 'opencode-go',
    harnessId: 'opencode',
    exclusive: false,
    sourceUrl: 'https://opencode.ai/docs/providers',
  },
  {
    providerId: 'github-copilot',
    harnessId: 'copilot',
    exclusive: false,
    sourceUrl: 'https://docs.github.com/en/copilot',
  },
]

export const BENCHMARKS: CatalogBenchmark[] = [
  {
    benchmarkId: 'swe-bench-verified',
    name: 'SWE-bench Verified',
    description: 'Resolved rate on the curated SWE-bench Verified software-engineering set.',
    unit: 'percent',
    higherIsBetter: true,
    sourceUrl: 'https://www.swebench.com/',
  },
  {
    benchmarkId: 'terminal-bench',
    name: 'Terminal-Bench',
    description: 'Success rate on Terminal-Bench agentic terminal tasks.',
    unit: 'percent',
    higherIsBetter: true,
    sourceUrl: 'https://www.tbench.ai/',
  },
  {
    benchmarkId: 'aa-coding-index',
    name: 'Artificial Analysis Coding Index',
    description: 'Composite coding index published by Artificial Analysis.',
    unit: 'score',
    higherIsBetter: true,
    sourceUrl: 'https://artificialanalysis.ai/',
  },
  {
    benchmarkId: 'aa-agentic-index',
    name: 'Artificial Analysis Agentic Index',
    description: 'Composite agentic coding index published by Artificial Analysis.',
    unit: 'score',
    higherIsBetter: true,
    sourceUrl: 'https://artificialanalysis.ai/',
  },
]

/**
 * No score is seeded: a benchmark score needs a real benchmark source, and the
 * source adapters are not enabled. The table and pages still render an explicit
 * unknown state until a score exists.
 */
export const BENCHMARK_SCORES: CatalogBenchmarkScore[] = []

function labAlias(lab: string): string {
  return LAB_ALIASES[lab] ?? lab
}

function fallbackLab(providerId: string): string {
  return PROVIDER_LABS[providerId] ?? labAlias(providerId)
}

/** Lowercase slug form. Dots become dashes so `gpt-5.5` and `gpt-5-5` group. */
export function normalizeModelSlug(value: string): string {
  return value
    .toLowerCase()
    .replace(/:(?:free|beta|extended|thinking|online)$/g, '')
    .replace(/@[\w.-]+$/g, '')
    .replace(/[^a-z0-9.]+/g, '-')
    .replace(/\./g, '-')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '')
}

export function canonicalModelIdFor(
  providerId: string,
  modelId: string,
  sourceCanonicalId: string | null,
): string {
  const raw = sourceCanonicalId?.trim() || modelId
  const segments = raw.split('/').filter((segment) => segment.trim() !== '')
  const slug = normalizeModelSlug(segments[segments.length - 1] ?? raw)
  const lab =
    segments.length > 1
      ? labAlias(normalizeModelSlug(segments[0]))
      : fallbackLab(providerId)
  return `${lab}/${slug}`
}

/** The leaf family for a canonical model: the longest matching family id. */
export function familyIdForCanonical(canonicalModelId: string): string | null {
  const slug = canonicalModelId.split('/').pop() ?? canonicalModelId
  let match: CatalogFamily | null = null
  for (const family of FAMILY_TREE) {
    if (!slug.includes(family.familyId)) {
      continue
    }
    if (
      match === null ||
      family.familyId.length > match.familyId.length ||
      (family.familyId.length === match.familyId.length && family.familyId < match.familyId)
    ) {
      match = family
    }
  }
  return match?.familyId ?? null
}

/**
 * Parents of parents come from each parent family's own `parentFamilyId`.
 * Returns the chain leaf-first and stops at the root.
 */
export function familyChain(
  familyId: string,
  families: CatalogFamily[] = FAMILY_TREE,
): CatalogFamily[] {
  const byId = new Map(families.map((family) => [family.familyId, family]))
  const chain: CatalogFamily[] = []
  const seen = new Set<string>()
  let current: CatalogFamily | undefined = byId.get(familyId)
  while (current) {
    if (seen.has(current.familyId)) {
      throw new Error(`Family tree contains a cycle at ${current.familyId}`)
    }
    seen.add(current.familyId)
    chain.push(current)
    current = current.parentFamilyId ? byId.get(current.parentFamilyId) : undefined
  }
  return chain
}

function labName(lab: string): string {
  const known = LAB_NAMES[lab]
  if (known) {
    return known
  }
  return lab
    .split('-')
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

function humanizeSlug(slug: string): string {
  return slug
    .split('-')
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

export interface CatalogEnrichment {
  models: CatalogModel[]
  canonicalModels: CatalogCanonicalModel[]
  families: CatalogFamily[]
  companies: CatalogCompany[]
  companyPlans: CatalogCompanyPlan[]
  harnesses: CatalogHarness[]
  providerHarnesses: CatalogProviderHarness[]
  benchmarks: CatalogBenchmark[]
  benchmarkScores: CatalogBenchmarkScore[]
}

/**
 * Attach canonical identity, family and harness facts to the fetched records.
 * Every provider serving a canonical model keeps its own model row; the
 * canonical row is what correlates them.
 */
export function enrichCatalog(
  providers: CatalogProvider[],
  models: CatalogModel[],
): CatalogEnrichment {
  const families = new Map(FAMILY_TREE.map((family) => [family.familyId, family]))
  const canonicalModels = new Map<string, CatalogCanonicalModel>()

  const enrichedModels = models.map((model) => {
    const canonicalModelId = canonicalModelIdFor(
      model.providerId,
      model.modelId,
      model.canonicalModelId || null,
    )
    const familyId = familyIdForCanonical(canonicalModelId) ?? canonicalModelId.split('/')[0]
    if (!families.has(familyId)) {
      families.set(familyId, {
        familyId,
        name: labName(familyId),
        parentFamilyId: null,
        vendor: familyId,
      })
    }
    if (!canonicalModels.has(canonicalModelId)) {
      const lab = canonicalModelId.split('/')[0]
      canonicalModels.set(canonicalModelId, {
        canonicalModelId,
        name: model.name ?? humanizeSlug(canonicalModelId.split('/').pop() ?? canonicalModelId),
        lab,
        familyId,
      })
    }
    return { ...model, canonicalModelId, familyId }
  })

  const providerIds = new Set(providers.map((provider) => provider.providerId))
  const canonicalIds = new Set(canonicalModels.keys())
  const benchmarkIds = new Set(BENCHMARKS.map((benchmark) => benchmark.benchmarkId))
  const companies = COMPANIES.filter((company) =>
    COMPANY_PLANS.some(
      (plan) =>
        plan.companyId === company.companyId &&
        (plan.providerId === null || providerIds.has(plan.providerId)),
    ),
  )
  const companyIds = new Set(companies.map((company) => company.companyId))

  return {
    models: enrichedModels,
    companies,
    companyPlans: COMPANY_PLANS.filter((plan) => companyIds.has(plan.companyId)).map((plan) => ({
      ...plan,
      providerId: plan.providerId !== null && providerIds.has(plan.providerId) ? plan.providerId : null,
    })),
    canonicalModels: [...canonicalModels.values()].sort((left, right) =>
      left.canonicalModelId.localeCompare(right.canonicalModelId),
    ),
    families: [...families.values()].sort((left, right) =>
      left.familyId.localeCompare(right.familyId),
    ),
    harnesses: HARNESSES,
    providerHarnesses: PROVIDER_HARNESS_LINKS.filter((link) => providerIds.has(link.providerId)),
    benchmarks: BENCHMARKS,
    benchmarkScores: BENCHMARK_SCORES.filter(
      (score) =>
        canonicalIds.has(score.canonicalModelId) && benchmarkIds.has(score.benchmarkId),
    ),
  }
}
