import { describe, expect, it } from 'vitest'
import { cn } from '../src/lib/cn'

describe('cn', () => {
  it('joins strings and numbers', () => {
    expect(cn('a', 'b', 3)).toBe('a b 3')
  })

  it('drops falsy values', () => {
    expect(cn('a', null, undefined, false, '', 'b')).toBe('a b')
  })

  it('flattens nested arrays', () => {
    expect(cn('a', ['b', ['c', false]], 'd')).toBe('a b c d')
  })

  it('includes record keys only when truthy', () => {
    expect(cn({ a: true, b: false, c: undefined }, 'd')).toBe('a d')
  })
})
