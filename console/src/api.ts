export type ItemState = 'open' | 'claimed' | 'resolved' | 'closed'
export type Resolution = 'done' | 'duplicate' | 'wontfix' | 'invalid'
export type Turn = 'requester' | 'assignee'

// Mirrors docket-core's Item exactly (crates/docket-core/src/domain.rs) —
// field names and casing are the wire format, not renamed to camelCase.
export interface Item {
  id: string
  topic: string
  title: string
  body: string | null
  state: ItemState
  resolution: Resolution | null
  /** Who this item is being worked for. Optional, set at creation only. */
  requester: string | null
  /** The worker currently holding the item (was `owner`). */
  assignee: string | null
  /** Derived from `state`, not stored — see ADR-0011. */
  turn: Turn | null
  tags: string[]
  created_at: number
  updated_at: number
}

// `assignee` is only set once a worker actually claims the item — before
// that, the closest honest answer to "who should look at this" is whoever
// owns the item's topic. The API stays honest (a null `assignee` means "not
// yet specifically assigned"); this is purely a display-layer fallback, kept
// next to Item so every consumer renders the same thing rather than each
// reinventing `item.assignee ?? item.topic`.
export function assigneeDisplay(item: Item): { value: string; isFallback: boolean } {
  return item.assignee
    ? { value: item.assignee, isFallback: false }
    : { value: item.topic, isFallback: true }
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

// docket-core reports a conflict (e.g. "cannot claim: item is claimed") as
// `{error: string}` — surfaced verbatim instead of a generic status code so
// the console can show the actual reason inline.
async function parseErrorMessage(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { error?: string }
    if (typeof body.error === 'string') return body.error
  } catch {
    // Response body wasn't JSON — fall through to the generic message.
  }
  return `request failed: ${res.status}`
}

async function mutate<T>(url: string, init: RequestInit): Promise<T> {
  const res = await fetch(url, init)
  if (!res.ok) {
    throw new Error(await parseErrorMessage(res))
  }
  return res.json() as Promise<T>
}

export async function claimItem(id: string, workerId: string): Promise<Item> {
  return mutate<Item>(`/api/items/${id}/claim`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ worker_id: workerId }),
  })
}

export async function submitItem(id: string, workerId: string): Promise<Item> {
  return mutate<Item>(`/api/items/${id}/submit`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ worker_id: workerId }),
  })
}

// The four closing operations record who closed the item (ADR-0012). The
// console has no per-user identity — every write from here is a button click
// in the single-owner admin UI — so it attributes them to a fixed `console`
// author rather than leaving the server's `"unknown"` fallback to stand in.
const CONSOLE_AUTHOR = 'console'

function authoredPost(): RequestInit {
  return {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ author: CONSOLE_AUTHOR }),
  }
}

export async function approveItem(id: string): Promise<Item> {
  return mutate<Item>(`/api/items/${id}/approve`, authoredPost())
}

// Admin operations (architecture.md's admin-operation mapping) — close an
// item before it reaches `resolved` via `approveItem`. Owner-agnostic and
// valid from any non-closed state, unlike `approveItem`'s `resolved`-only
// gate: an admin can catch a mistaken/duplicate/irrelevant item at any point.
export async function removeItem(id: string): Promise<Item> {
  return mutate<Item>(`/api/items/${id}/remove`, authoredPost())
}

export async function mergeItem(id: string): Promise<Item> {
  return mutate<Item>(`/api/items/${id}/merge`, authoredPost())
}

export async function forceCloseItem(id: string): Promise<Item> {
  return mutate<Item>(`/api/items/${id}/force-close`, authoredPost())
}

export async function addItemTags(id: string, tags: string[]): Promise<string[]> {
  return mutate<string[]>(`/api/items/${id}/tags`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ tags }),
  })
}

export async function removeItemTags(id: string, tags: string[]): Promise<string[]> {
  return mutate<string[]>(`/api/items/${id}/tags`, {
    method: 'DELETE',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ tags }),
  })
}
