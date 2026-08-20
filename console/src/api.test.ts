import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  addItemTags,
  approveItem,
  archiveItem,
  claimItem,
  fetchComments,
  fetchItems,
  fetchTags,
  forceCloseItem,
  mergeItem,
  reopenItem,
  rejectItem,
  removeItem,
  removeItemTags,
  submitItem,
} from './api'

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
  it('always sends the server-cap limit, with no q param when called with no argument', async () => {
    mockFetchOnce([])

    await fetchItems()

    expect(fetch).toHaveBeenCalledWith('/api/items?limit=200')
  })

  it('omits q when the query is whitespace-only', async () => {
    mockFetchOnce([])

    await fetchItems('   ')

    expect(fetch).toHaveBeenCalledWith('/api/items?limit=200')
  })

  it('appends an encoded q param when a query is given', async () => {
    mockFetchOnce([])

    await fetchItems('race in claim_item')

    expect(fetch).toHaveBeenCalledWith('/api/items?limit=200&q=race+in+claim_item')
  })

  it('throws when the request fails', async () => {
    mockFetchOnce({}, false, 500)

    await expect(fetchItems()).rejects.toThrow('GET /api/items failed: 500')
  })
})

describe('claimItem', () => {
  it('POSTs the worker id and returns the updated item', async () => {
    const item = { id: 'i1', state: 'claimed', assignee: 'console' }
    mockFetchOnce(item)

    const result = await claimItem('i1', 'console')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith(
      '/api/items/i1/claim',
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ worker_id: 'console' }),
      }),
    )
  })

  it('throws the server error message on conflict', async () => {
    mockFetchOnce({ error: 'cannot claim: item is claimed' }, false, 409)

    await expect(claimItem('i1', 'console')).rejects.toThrow('cannot claim: item is claimed')
  })

  it('falls back to a generic message when the error body is not JSON', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: false,
        status: 500,
        json: () => Promise.reject(new Error('not json')),
      }),
    )

    await expect(claimItem('i1', 'console')).rejects.toThrow('request failed: 500')
  })
})

describe('submitItem', () => {
  it('POSTs the worker id and returns the updated item', async () => {
    const item = { id: 'i1', state: 'resolved' }
    mockFetchOnce(item)

    const result = await submitItem('i1', 'console')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith(
      '/api/items/i1/submit',
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ worker_id: 'console' }),
      }),
    )
  })
})

describe('approveItem', () => {
  it('POSTs a JSON author body and returns the updated item', async () => {
    const item = { id: 'i1', state: 'closed', resolution: 'done' }
    mockFetchOnce(item)

    const result = await approveItem('i1')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith(
      '/api/items/i1/approve',
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ author: 'console' }),
      }),
    )
  })
})

describe.each([
  ['removeItem', removeItem, 'remove'],
  ['forceCloseItem', forceCloseItem, 'force-close'],
] as const)('%s', (_name, fn, route) => {
  it('POSTs a JSON author body and returns the updated item', async () => {
    const item = { id: 'i1', state: 'closed' }
    mockFetchOnce(item)

    const result = await fn('i1')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith(
      `/api/items/i1/${route}`,
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ author: 'console' }),
      }),
    )
  })

  it('throws the server error message on conflict', async () => {
    mockFetchOnce({ error: `cannot ${route}: item is closed` }, false, 409)

    await expect(fn('i1')).rejects.toThrow(`cannot ${route}: item is closed`)
  })
})

describe('mergeItem', () => {
  it('POSTs a JSON author+duplicate_of_id body and returns the updated item', async () => {
    const item = { id: 'i1', state: 'closed', resolution: 'duplicate' }
    mockFetchOnce(item)

    const result = await mergeItem('i1', 'original-item-id')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith(
      '/api/items/i1/merge',
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ author: 'console', duplicate_of_id: 'original-item-id' }),
      }),
    )
  })

  it('throws the server error message on conflict', async () => {
    mockFetchOnce({ error: 'cannot merge: item is closed' }, false, 409)

    await expect(mergeItem('i1', 'original-item-id')).rejects.toThrow(
      'cannot merge: item is closed',
    )
  })
})

describe.each([
  ['rejectItem', rejectItem, 'reject'],
  ['reopenItem', reopenItem, 'reopen'],
] as const)('%s', (_name, fn, route) => {
  it('POSTs a JSON author+reason body and returns the updated item', async () => {
    const item = { id: 'i1', state: 'claimed' }
    mockFetchOnce(item)

    const result = await fn('i1', 'not ready yet')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith(
      `/api/items/i1/${route}`,
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ author: 'console', reason: 'not ready yet' }),
      }),
    )
  })

  it('throws the server error message on conflict', async () => {
    mockFetchOnce({ error: `cannot ${route}: item is open` }, false, 409)

    await expect(fn('i1', 'not ready yet')).rejects.toThrow(`cannot ${route}: item is open`)
  })
})

describe('archiveItem', () => {
  it('POSTs with no body and returns the updated item', async () => {
    const item = { id: 'i1', state: 'closed', archived_at: 1234 }
    mockFetchOnce(item)

    const result = await archiveItem('i1')

    expect(result).toEqual(item)
    expect(fetch).toHaveBeenCalledWith('/api/items/i1/archive', { method: 'POST' })
  })
})

describe('addItemTags', () => {
  it('POSTs the tags and returns the full tag set', async () => {
    mockFetchOnce(['a', 'b'])

    const result = await addItemTags('i1', ['b'])

    expect(result).toEqual(['a', 'b'])
    expect(fetch).toHaveBeenCalledWith(
      '/api/items/i1/tags',
      expect.objectContaining({
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ tags: ['b'] }),
      }),
    )
  })
})

describe('removeItemTags', () => {
  it('DELETEs the tags and returns the remaining tag set', async () => {
    mockFetchOnce(['a'])

    const result = await removeItemTags('i1', ['b'])

    expect(result).toEqual(['a'])
    expect(fetch).toHaveBeenCalledWith(
      '/api/items/i1/tags',
      expect.objectContaining({
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ tags: ['b'] }),
      }),
    )
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
