import { useEffect, useState } from 'react'
import { fetchItems, type Item } from './api'

export interface UseItemsResult {
  items: Item[]
  connected: boolean
  loading: boolean
}

const POLL_INTERVAL_MS = 5000

// Keeps the last successfully fetched items on the screen when a poll
// fails — `connected` flips to false so the caller can show a banner,
// but the board itself never goes blank on a transient failure.
//
// `query` is forwarded to every poll, so changing it (via its effect
// dependency) re-fetches immediately and restarts the interval — same
// mechanism `intervalMs` already used.
export function useItems(
  query: string = '',
  intervalMs: number = POLL_INTERVAL_MS,
): UseItemsResult {
  const [items, setItems] = useState<Item[]>([])
  const [connected, setConnected] = useState(true)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    let cancelled = false

    async function poll() {
      try {
        const next = await fetchItems(query)
        if (!cancelled) {
          setItems(next)
          setConnected(true)
        }
      } catch {
        if (!cancelled) setConnected(false)
      } finally {
        if (!cancelled) setLoading(false)
      }
    }

    poll()
    const id = setInterval(poll, intervalMs)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [query, intervalMs])

  return { items, connected, loading }
}
