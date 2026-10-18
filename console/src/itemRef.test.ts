import { describe, expect, it } from 'vitest'
import type { Item } from './api'
import { matchesItemRef, parseSeqRef } from './itemRef'

const item = { id: '00000000-0000-4000-8000-000000000083', seq: 83 } as Item

describe('parseSeqRef', () => {
  it('reads a bare number and the #-prefixed form', () => {
    expect(parseSeqRef('83')).toBe(83)
    expect(parseSeqRef('#83')).toBe(83)
    expect(parseSeqRef(' #83 ')).toBe(83)
  })

  it('rejects anything else', () => {
    expect(parseSeqRef('83a')).toBeNull()
    expect(parseSeqRef('# 83')).toBeNull()
    expect(parseSeqRef('')).toBeNull()
    expect(parseSeqRef(item.id)).toBeNull()
  })
})

describe('matchesItemRef', () => {
  it('matches by id or by seq', () => {
    expect(matchesItemRef(item, item.id)).toBe(true)
    expect(matchesItemRef(item, '83')).toBe(true)
    expect(matchesItemRef(item, '#83')).toBe(true)
  })

  it('does not match another seq', () => {
    expect(matchesItemRef(item, '84')).toBe(false)
  })
})
