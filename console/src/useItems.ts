import { useCallback, useEffect, useState } from 'react'
import { fetchItems, UnauthorizedError, type Item } from './api'

export interface UseItemsResult {
  items: Item[]
  connected: boolean
  /** The server wants an API token (ADR-0026) and has none, or a wrong one. */
  unauthorized: boolean
  loading: boolean
  /** Re-polls immediately and restarts the interval — call after a
   * successful mutation (claim/submit/approve/tag) so its effect shows up
   * without waiting for the next scheduled poll. */
  refresh: () => void
}

const POLL_INTERVAL_MS = 5000

// Keeps the last successfully fetched items on the screen when a poll
// fails — `connected` flips to false so the caller can show a banner,
// but the board itself never goes blank on a transient failure.
//
// `query`/`archived` are forwarded to every poll, so changing either (via
// the effect dependency) re-fetches immediately and restarts the interval —
// same mechanism `intervalMs` already used.
export function useItems(
  query: string = '',
  archived: boolean = false,
  intervalMs: number = POLL_INTERVAL_MS,
): UseItemsResult {
  const [items, setItems] = useState<Item[]>([])
  const [connected, setConnected] = useState(true)
  const [unauthorized, setUnauthorized] = useState(false)
  const [loading, setLoading] = useState(true)
  const [refreshNonce, setRefreshNonce] = useState(0)

  useEffect(() => {
    let cancelled = false

    async function poll() {
      try {
        const next = await fetchItems(query, archived)
        if (!cancelled) {
          setItems(next)
          setConnected(true)
          setUnauthorized(false)
        }
      } catch (err) {
        if (!cancelled) {
          // A refused token is not an unreachable server: keep the two apart
          // so the banner can say what to do.
          setUnauthorized(err instanceof UnauthorizedError)
          setConnected(false)
        }
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
  }, [query, archived, intervalMs, refreshNonce])

  const refresh = useCallback(() => setRefreshNonce((n) => n + 1), [])

  return { items, connected, unauthorized, loading, refresh }
}
