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
  created_at: number
  updated_at: number
}

export async function fetchItems(): Promise<Item[]> {
  const res = await fetch('/api/items')
  if (!res.ok) {
    throw new Error(`GET /api/items failed: ${res.status}`)
  }
  return res.json() as Promise<Item[]>
}
