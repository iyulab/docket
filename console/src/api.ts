export type ItemState = 'open' | 'claimed' | 'resolved' | 'closed'
export type Resolution = 'done' | 'duplicate' | 'wontfix' | 'invalid'

// Mirrors docket-core's Item exactly (crates/docket-core/src/domain.rs) —
// field names and casing are the wire format, not renamed to camelCase.
export interface Item {
  id: string
  topic: string
  title: string
  body: string | null
  state: ItemState
  resolution: Resolution | null
  owner: string | null
  tags: string[]
  created_at: number
  updated_at: number
}

export interface Comment {
  id: string
  item_id: string
  author: string
  body: string
  created_at: number
}

export interface TagCount {
  tag: string
  count: number
}

export async function fetchItems(query?: string): Promise<Item[]> {
  const trimmed = query?.trim()
  const url = trimmed ? `/api/items?q=${encodeURIComponent(trimmed)}` : '/api/items'
  const res = await fetch(url)
  if (!res.ok) {
    throw new Error(`GET /api/items failed: ${res.status}`)
  }
  return res.json() as Promise<Item[]>
}

export async function fetchComments(itemId: string): Promise<Comment[]> {
  const res = await fetch(`/api/items/${itemId}/comments`)
  if (!res.ok) {
    throw new Error(`GET /api/items/${itemId}/comments failed: ${res.status}`)
  }
  return res.json() as Promise<Comment[]>
}

export async function fetchTags(): Promise<TagCount[]> {
  const res = await fetch('/api/tags')
  if (!res.ok) {
    throw new Error(`GET /api/tags failed: ${res.status}`)
  }
  return res.json() as Promise<TagCount[]>
}
