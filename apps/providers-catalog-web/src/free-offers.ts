import type { CatalogFreeOffer } from './types'

export interface FreeOfferMergeReport {
  input: number
  written: number
  mergedOfferIds: string[]
}

export interface NormalizedFreeOffers {
  offers: CatalogFreeOffer[]
  report: FreeOfferMergeReport
}

export class FreeOfferPolicyError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'FreeOfferPolicyError'
  }
}

interface FreeOfferClaim {
  offerId: string
  offerKind: string
  allowanceUnit: string
  resetPeriod: string
  eligibility: string | null
  conditions: string | null
}

const EXCLUDED_CLAIM_PATTERNS: { pattern: RegExp; reason: string }[] = [
  {
    pattern: /\b(?:one[-\s]?time|one[-\s]?off|single[-\s]?use)\b/,
    reason: 'one-time credit or grant',
  },
  {
    pattern: /\b(?:sign[-\s]?up|signup|launch|welcome|introductory)\s+(?:credit|bonus|grant|offer)\b/,
    reason: 'signup or launch credit',
  },
  {
    pattern: /\btrial\b/,
    reason: 'trial',
  },
  {
    pattern: /\b(?:expires?|expiring|expiration|valid until|ends? on)\b/,
    reason: 'expiring claim',
  },
  {
    pattern: /\blimited[-\s]?time\b/,
    reason: 'limited-time promotion',
  },
  {
    pattern: /\bfirst\s+(?:month|30 days)\b/,
    reason: 'introductory period',
  },
  {
    pattern: /\bfree\s+for\s+(?:\d+|one|two|three|seven|fourteen|thirty)\s+days\b/,
    reason: 'temporary free period',
  },
  {
    pattern: /\bpromotion(?:al)?\b/,
    reason: 'promotion',
  },
]

export function freeOfferPolicyViolation(offer: FreeOfferClaim): string | null {
  if (offer.offerKind !== 'recurring') {
    return `offer kind is ${offer.offerKind}, not recurring`
  }

  const text = [
    offer.eligibility ?? '',
    offer.conditions ?? '',
    offer.resetPeriod,
    offer.allowanceUnit,
  ]
    .join(' ')
    .toLowerCase()
    .replace(/\b(?:no|not|without)\s+(?:a\s+)?trial\b/g, ' ')
    .replace(/\b(?:no|without)\s+(?:expiration|expiry)\b/g, ' ')
    .replace(/\bnever\s+expires?\b/g, ' ')

  for (const { pattern, reason } of EXCLUDED_CLAIM_PATTERNS) {
    if (pattern.test(text)) {
      return reason
    }
  }
  return null
}

export function normalizeFreeOffers(offers: CatalogFreeOffer[]): NormalizedFreeOffers {
  const groups = new Map<string, CatalogFreeOffer[]>()

  for (const offer of offers) {
    const violation = freeOfferPolicyViolation(offer)
    if (violation) {
      throw new FreeOfferPolicyError(`Free offer ${offer.offerId} is excluded: ${violation}`)
    }

    const key = offerKey(offer)
    const group = groups.get(key)
    if (group) {
      group.push(offer)
    } else {
      groups.set(key, [offer])
    }
  }

  const normalized: CatalogFreeOffer[] = []
  const mergedOfferIds = new Set<string>()

  for (const group of groups.values()) {
    const [canonical, ...duplicates] = [...group].sort(preferredOffer)
    for (const duplicate of duplicates) {
      if (
        duplicate.eligibility !== canonical.eligibility ||
        duplicate.conditions !== canonical.conditions
      ) {
        throw new FreeOfferPolicyError(
          `Free offers ${canonical.offerId} and ${duplicate.offerId} share an allowance but have conflicting conditions`,
        )
      }
      mergedOfferIds.add(duplicate.offerId)
    }
    normalized.push(canonical)
  }

  normalized.sort((left, right) => left.offerId.localeCompare(right.offerId))

  return {
    offers: normalized,
    report: {
      input: offers.length,
      written: normalized.length,
      mergedOfferIds: [...mergedOfferIds].sort(),
    },
  }
}

function offerKey(offer: CatalogFreeOffer): string {
  return [
    offer.providerId,
    offer.planId ?? '',
    offer.allowance,
    offer.allowanceUnit,
    offer.resetPeriod,
  ].join('\u0000')
}

function preferredOffer(left: CatalogFreeOffer, right: CatalogFreeOffer): number {
  const leftVerifiedAt = Date.parse(left.verifiedAt)
  const rightVerifiedAt = Date.parse(right.verifiedAt)
  if (Number.isNaN(leftVerifiedAt) || Number.isNaN(rightVerifiedAt)) {
    throw new FreeOfferPolicyError('Free offers must have a valid verified_at timestamp')
  }

  const recency = rightVerifiedAt - leftVerifiedAt
  return recency === 0 ? left.offerId.localeCompare(right.offerId) : recency
}
