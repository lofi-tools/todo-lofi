import type { CatalogData } from './catalog'

export const RANKING_FORMULA =
  'monthly recurring free requests or tokens × mean coding-benchmark score of the models the harness can use'

/**
 * Every curated benchmark measures coding or agentic-coding ability, so all of
 * them feed the ranking. A score is averaged per canonical model first, then
 * across the models a harness can reach.
 */
export const CODING_BENCHMARK_IDS = [
  'swe-bench-verified',
  'terminal-bench',
  'aa-coding-index',
  'aa-agentic-index',
] as const

export type FreeAllowanceUnit = 'requests' | 'tokens'

export interface HarnessFreeAllowance {
  unit: FreeAllowanceUnit
  monthlyAmount: number
  offerCount: number
}

export interface HarnessRankingEntry {
  harnessId: string
  harnessName: string
  providerIds: string[]
  providerNames: string[]
  freeAllowances: HarnessFreeAllowance[]
  averageCodingScore: number | null
  scoredModels: number
  score: number | null
}

export interface HarnessRankingGroup {
  unit: FreeAllowanceUnit
  entries: HarnessRankingEntry[]
}

export interface HarnessRanking {
  snapshotId: string
  generatedAt: string
  stale: boolean
  formula: string
  benchmarkIds: string[]
  totalHarnesses: number
  totalRecurringOffers: number
  totalBenchmarkScores: number
  entries: HarnessRankingEntry[]
  groups: HarnessRankingGroup[]
}

const MONTHLY_FACTOR: Record<string, number> = {
  minute: 60 * 24 * 30.4375,
  hour: 24 * 30.4375,
  day: 30.4375,
  week: 30.4375 / 7,
  month: 1,
  year: 1 / 12,
}

function monthlyFactor(resetPeriod: string): number | null {
  const key = resetPeriod.trim().toLowerCase().replace(/s$/, '')
  return MONTHLY_FACTOR[key] ?? null
}

function round(value: number): number {
  return Math.round(value * 100) / 100
}

function mean(values: number[]): number | null {
  if (values.length === 0) {
    return null
  }
  return round(values.reduce((total, value) => total + value, 0) / values.length)
}

function freeUnit(unit: string): FreeAllowanceUnit | null {
  return unit === 'requests' || unit === 'tokens' ? unit : null
}

/**
 * Requests and tokens are not interchangeable, so a harness is ranked within
 * each allowance unit instead of against a combined number.
 */
export function buildHarnessRanking(catalog: CatalogData): HarnessRanking {
  const providerName = new Map(catalog.providers.map((provider) => [provider.providerId, provider.name]))
  const benchmarkIds = new Set<string>(CODING_BENCHMARK_IDS)
  const scoresByCanonical = new Map<string, number[]>()
  for (const score of catalog.benchmarkScores) {
    if (!benchmarkIds.has(score.benchmarkId)) {
      continue
    }
    const scores = scoresByCanonical.get(score.canonicalModelId) ?? []
    scores.push(score.score)
    scoresByCanonical.set(score.canonicalModelId, scores)
  }

  const monthlyAmounts = new Map<string, number>()
  const entries = catalog.harnesses.map((harness): HarnessRankingEntry => {
    const providerIds = [
      ...new Set(
        catalog.providerHarnesses
          .filter((link) => link.harnessId === harness.harnessId)
          .map((link) => link.providerId),
      ),
    ]
    const providerIdSet = new Set(providerIds)

    const allowances = new Map<FreeAllowanceUnit, { amount: number; count: number }>()
    for (const offer of catalog.offers) {
      const unit = freeUnit(offer.allowanceUnit)
      const factor = monthlyFactor(offer.resetPeriod)
      if (!providerIdSet.has(offer.providerId) || unit === null || factor === null) {
        continue
      }
      const current = allowances.get(unit) ?? { amount: 0, count: 0 }
      allowances.set(unit, {
        amount: current.amount + offer.allowance * factor,
        count: current.count + 1,
      })
    }

    for (const [unit, allowance] of allowances) {
      monthlyAmounts.set(`${harness.harnessId}|${unit}`, allowance.amount)
    }

    const canonicalIds = new Set(
      catalog.models
        .filter((model) => providerIdSet.has(model.providerId))
        .map((model) => model.canonicalModelId),
    )
    const modelAverages: number[] = []
    for (const canonicalModelId of canonicalIds) {
      const average = mean(scoresByCanonical.get(canonicalModelId) ?? [])
      if (average !== null) {
        modelAverages.push(average)
      }
    }

    return {
      harnessId: harness.harnessId,
      harnessName: harness.name,
      providerIds,
      providerNames: providerIds.map((providerId) => providerName.get(providerId) ?? providerId),
      freeAllowances: [...allowances.entries()]
        .map(([unit, allowance]) => ({
          unit,
          monthlyAmount: round(allowance.amount),
          offerCount: allowance.count,
        }))
        .sort((left, right) => left.unit.localeCompare(right.unit)),
      averageCodingScore: mean(modelAverages),
      scoredModels: modelAverages.length,
      score: null,
    }
  })

  const groups: HarnessRankingGroup[] = []
  for (const unit of ['requests', 'tokens'] as const) {
    const ranked = entries
      .flatMap((entry) => {
        // Score from the unrounded monthly amount so the display rounding of
        // `monthlyAmount` does not change the ranking.
        const monthlyAmount = monthlyAmounts.get(`${entry.harnessId}|${unit}`)
        if (monthlyAmount === undefined || entry.averageCodingScore === null) {
          return []
        }
        return [
          {
            ...entry,
            score: round(monthlyAmount * entry.averageCodingScore),
          },
        ]
      })
      .sort(
        (left, right) =>
          (right.score ?? 0) - (left.score ?? 0) ||
          left.harnessName.localeCompare(right.harnessName),
      )
    if (ranked.length > 0) {
      groups.push({ unit, entries: ranked })
    }
  }

  return {
    snapshotId: catalog.snapshot.snapshotId,
    generatedAt: catalog.snapshot.generatedAt,
    stale: catalog.snapshot.stale,
    formula: RANKING_FORMULA,
    benchmarkIds: [...CODING_BENCHMARK_IDS],
    totalHarnesses: catalog.harnesses.length,
    totalRecurringOffers: catalog.offers.length,
    totalBenchmarkScores: catalog.benchmarkScores.length,
    entries,
    groups,
  }
}
