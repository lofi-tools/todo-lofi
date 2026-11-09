import { describe, expect, it } from 'vitest'
import {
  FreeOfferPolicyError,
  freeOfferPolicyViolation,
  normalizeFreeOffers,
} from '../src/free-offers'
import { fixtureFreeOffer } from './fixtures'

describe('free offers', () => {
  it('merges exact duplicate claims deterministically', () => {
    const result = normalizeFreeOffers([
      fixtureFreeOffer({ offerId: 'offer-2', sourceRecord: 'acme/offer-2' }),
      fixtureFreeOffer(),
    ])

    expect(result.report).toEqual({
      input: 2,
      written: 1,
      mergedOfferIds: ['offer-2'],
    })
    expect(result.offers).toEqual([fixtureFreeOffer()])
  })

  it('rejects one-time and expiring trial claims', () => {
    expect(
      freeOfferPolicyViolation(
        fixtureFreeOffer({ conditions: 'One-time signup credit, expires in 30 days.' }),
      ),
    ).toBe('one-time credit or grant')
    expect(
      freeOfferPolicyViolation(fixtureFreeOffer({ conditions: 'Free for 30 days.' })),
    ).toBe('temporary free period')
    expect(freeOfferPolicyViolation(fixtureFreeOffer({ conditions: 'Limited-time trial.' }))).toBe(
      'trial',
    )
  })

  it('accepts recurring claims that explicitly exclude a trial or expiration', () => {
    expect(freeOfferPolicyViolation(fixtureFreeOffer({ conditions: 'No trial required.' }))).toBeNull()
    expect(freeOfferPolicyViolation(fixtureFreeOffer({ conditions: 'Never expires.' }))).toBeNull()
  })

  it('fails closed when duplicates have conflicting conditions', () => {
    expect(() =>
      normalizeFreeOffers([
        fixtureFreeOffer(),
        fixtureFreeOffer({
          offerId: 'offer-2',
          conditions: 'New accounts only.',
          sourceRecord: 'acme/offer-2',
        }),
      ]),
    ).toThrow(FreeOfferPolicyError)
  })
})
