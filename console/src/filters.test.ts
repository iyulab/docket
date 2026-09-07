import { describe, expect, it } from 'vitest'
import type { Item } from './api'
import { deriveTopics, identityEq, matchesFilters, relationOf, sortItems } from './filters'

function makeItem(overrides: Partial<Item> = {}): Item {
  return {
    id: 'i1',
    seq: 1,
    topic: 'iyulab/docket',
    title: 'title',
    body: null,
    state: 'open',
    resolution: null,
    requester: null,
    assignee: null,
    turn: null,
    open: true,
    tags: [],
    created_at: 1000,
    updated_at: 1000,
    archived_at: null,
    ...overrides,
  }
}

describe('relationOf', () => {
  it('returns "to" when the item belongs to the perspective topic', () => {
    const item = makeItem({ topic: 'iyulab/docket' })
    expect(relationOf(item, 'iyulab/docket')).toBe('to')
  })

  it('returns "from" when the item is tagged found-in: the perspective topic', () => {
    const item = makeItem({ topic: 'iyulab/docket', tags: ['found-in:iyulab/router'] })
    expect(relationOf(item, 'iyulab/router')).toBe('from')
  })

  it('returns null when the item is unrelated to the perspective topic', () => {
    const item = makeItem({ topic: 'iyulab/docket', tags: ['found-in:iyulab/router'] })
    expect(relationOf(item, 'iyulab/other')).toBeNull()
  })

  it('returns "from" for any of multiple found-in tags on the same item', () => {
    const item = makeItem({
      topic: 'iyulab/docket',
      tags: ['found-in:iyulab/router', 'found-in:iyulab/EDMS-v2'],
    })
    expect(relationOf(item, 'iyulab/EDMS-v2')).toBe('from')
  })

  it('returns "from" when item.requester matches the perspective topic, with no found-in tag', () => {
    const item = makeItem({ topic: 'iyulab/docket', requester: 'iyulab/router', tags: [] })
    expect(relationOf(item, 'iyulab/router')).toBe('from')
  })

  it('still returns "from" via the legacy found-in tag when requester is unset', () => {
    const item = makeItem({
      topic: 'iyulab/docket',
      requester: null,
      tags: ['found-in:iyulab/router'],
    })
    expect(relationOf(item, 'iyulab/router')).toBe('from')
  })
})

describe('matchesFilters', () => {
  const noFilter = { states: [], tags: [], perspectiveTopic: null, relation: 'all' as const }

  it('passes everything when no filter is active', () => {
    expect(matchesFilters(makeItem(), noFilter)).toBe(true)
  })

  it('filters by state', () => {
    const item = makeItem({ state: 'closed' })
    expect(matchesFilters(item, { ...noFilter, states: ['open', 'claimed'] })).toBe(false)
    expect(matchesFilters(item, { ...noFilter, states: ['closed'] })).toBe(true)
  })

  it('filters by tag (item must carry every listed tag)', () => {
    const item = makeItem({ tags: ['blocked', 'deferred'] })
    expect(matchesFilters(item, { ...noFilter, tags: ['blocked'] })).toBe(true)
    expect(matchesFilters(item, { ...noFilter, tags: ['blocked', 'missing'] })).toBe(false)
  })

  it('ignores relation when the toggle is "all", even with a perspective set', () => {
    const item = makeItem({ topic: 'iyulab/docket' })
    expect(
      matchesFilters(item, { ...noFilter, perspectiveTopic: 'iyulab/other', relation: 'all' }),
    ).toBe(true)
  })

  it('filters to only "to" items when relation is "to"', () => {
    const toItem = makeItem({ topic: 'iyulab/docket' })
    const unrelated = makeItem({ topic: 'iyulab/other' })
    const filters = { ...noFilter, perspectiveTopic: 'iyulab/docket', relation: 'to' as const }
    expect(matchesFilters(toItem, filters)).toBe(true)
    expect(matchesFilters(unrelated, filters)).toBe(false)
  })

  it('filters to only "from" items when relation is "from"', () => {
    const fromItem = makeItem({ topic: 'iyulab/docket', tags: ['found-in:iyulab/router'] })
    const toItem = makeItem({ topic: 'iyulab/router' })
    const filters = { ...noFilter, perspectiveTopic: 'iyulab/router', relation: 'from' as const }
    expect(matchesFilters(fromItem, filters)).toBe(true)
    expect(matchesFilters(toItem, filters)).toBe(false)
  })

  it('matches "from" via item.requester alone, no found-in tag needed', () => {
    const fromItem = makeItem({ topic: 'iyulab/docket', requester: 'iyulab/router', tags: [] })
    const filters = { ...noFilter, perspectiveTopic: 'iyulab/router', relation: 'from' as const }
    expect(matchesFilters(fromItem, filters)).toBe(true)
  })
})

describe('sortItems', () => {
  it('sorts by updated_at descending', () => {
    const a = makeItem({ id: 'a', updated_at: 100 })
    const b = makeItem({ id: 'b', updated_at: 300 })
    const c = makeItem({ id: 'c', updated_at: 200 })
    expect(sortItems([a, b, c], 'updated_at').map((i) => i.id)).toEqual(['b', 'c', 'a'])
  })

  it('sorts by created_at descending', () => {
    const a = makeItem({ id: 'a', created_at: 100 })
    const b = makeItem({ id: 'b', created_at: 300 })
    expect(sortItems([a, b], 'created_at').map((i) => i.id)).toEqual(['b', 'a'])
  })

  it('sorts by workflow state order (open, claimed, resolved, closed)', () => {
    const closed = makeItem({ id: 'closed', state: 'closed' })
    const open = makeItem({ id: 'open', state: 'open' })
    const claimed = makeItem({ id: 'claimed', state: 'claimed' })
    expect(sortItems([closed, open, claimed], 'state').map((i) => i.id)).toEqual([
      'open',
      'claimed',
      'closed',
    ])
  })

  it('does not mutate the input array', () => {
    const items = [makeItem({ id: 'a', updated_at: 1 }), makeItem({ id: 'b', updated_at: 2 })]
    const original = [...items]
    sortItems(items, 'updated_at')
    expect(items).toEqual(original)
  })
})

describe('deriveTopics', () => {
  it('collects each item topic plus found-in: tag targets, deduplicated and sorted', () => {
    const items = [
      makeItem({ topic: 'iyulab/router', tags: ['found-in:iyulab/docket'] }),
      makeItem({ topic: 'iyulab/docket', tags: ['found-in:iyulab/router', 'blocked'] }),
    ]
    expect(deriveTopics(items)).toEqual(['iyulab/docket', 'iyulab/router'])
  })

  it('returns an empty list for no items', () => {
    expect(deriveTopics([])).toEqual([])
  })

  it('also collects item.requester, even with no found-in tag', () => {
    const items = [makeItem({ topic: 'iyulab/docket', requester: 'iyulab/router', tags: [] })]
    expect(deriveTopics(items)).toEqual(['iyulab/docket', 'iyulab/router'])
  })
})

describe('identity comparison (ADR-0021)', () => {
  it('folds ASCII case, so the console classifies an item the way the server does', () => {
    expect(identityEq('iyulab/Filer', 'iyulab/filer')).toBe(true)
    expect(identityEq('iyu-devstack/Schemorph', 'iyu-devstack/schemorph')).toBe(true)
    expect(identityEq('iyulab/Filer', 'iyulab/Filer2')).toBe(false)
  })

  it('leaves non-ASCII byte-exact, matching the server rather than out-folding it', () => {
    expect(identityEq('acme/grün', 'acme/GRÜN')).toBe(false)
  })

  it('relationOf matches a perspective topic whose case drifted', () => {
    const to = makeItem({ topic: 'iyulab/Docket' })
    expect(relationOf(to, 'iyulab/docket')).toBe('to')

    const from = makeItem({ topic: 'iyulab/docket', requester: 'iyulab/Filer' })
    expect(relationOf(from, 'iyulab/filer')).toBe('from')

    const legacy = makeItem({ topic: 'iyulab/docket', tags: ['found-in:iyulab/Filer'] })
    expect(relationOf(legacy, 'iyulab/filer')).toBe('from')
  })

  it('deriveTopics collapses one identity into a single picker entry', () => {
    const topics = deriveTopics([
      makeItem({ topic: 'iyulab/docket', requester: 'iyulab/Filer' }),
      makeItem({ topic: 'iyulab/docket', requester: 'iyulab/filer' }),
    ])
    expect(topics).toEqual(['iyulab/Filer', 'iyulab/docket'])
  })
})
