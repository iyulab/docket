import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchComments, fetchItems, fetchTags } from './api'

function mockFetchOnce(body: unknown, ok = true, status = 200) {
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue({
      ok,
      status,
      json: () => Promise.resolve(body),
    }),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('fetchComments', () => {
  it('returns the comment list on success', async () => {
    const comments = [{ id: 'c1', item_id: 'i1', author: 'a', body: 'hi', created_at: 1 }]
    mockFetchOnce(comments)

    const result = await fetchComments('i1')

    expect(result).toEqual(comments)
    expect(fetch).toHaveBeenCalledWith('/api/items/i1/comments')
  })

  it('throws when the request fails', async () => {
    mockFetchOnce({}, false, 500)

    await expect(fetchComments('i1')).rejects.toThrow('GET /api/items/i1/comments failed: 500')
  })
})

describe('fetchItems', () => {
  it('fetches without a query param when called with no argument', async () => {
    mockFetchOnce([])

    await fetchItems()

    expect(fetch).toHaveBeenCalledWith('/api/items')
  })

  it('fetches without a query param when the query is whitespace-only', async () => {
    mockFetchOnce([])

    await fetchItems('   ')

    expect(fetch).toHaveBeenCalledWith('/api/items')
  })

  it('appends an encoded q param when a query is given', async () => {
    mockFetchOnce([])

    await fetchItems('race in claim_item')

    expect(fetch).toHaveBeenCalledWith('/api/items?q=race%20in%20claim_item')
  })

  it('throws when the request fails', async () => {
    mockFetchOnce({}, false, 500)

    await expect(fetchItems()).rejects.toThrow('GET /api/items failed: 500')
  })
})

describe('fetchTags', () => {
  it('returns the tag vocabulary on success', async () => {
    const tags = [{ tag: 'found-in:iyulab/docket', count: 3 }]
    mockFetchOnce(tags)

    const result = await fetchTags()

    expect(result).toEqual(tags)
    expect(fetch).toHaveBeenCalledWith('/api/tags')
  })

  it('throws when the request fails', async () => {
    mockFetchOnce({}, false, 500)

    await expect(fetchTags()).rejects.toThrow('GET /api/tags failed: 500')
  })
})
