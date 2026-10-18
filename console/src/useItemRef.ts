import { useEffect, useMemo, useState } from 'react'
import type { Item } from './api'
import { fetchItem } from './api'
import { matchesItemRef } from './itemRef'

// Resolves an item reference (`id`, `83` or `#83`) against the polled list
// first and asks docket-core only for an item the list does not hold. The list
// is capped and shaped by the current search, so a pasted reference to an
// older or filtered-out item would otherwise read as "not found". Re-runs with
// each poll, which keeps a fetched item as fresh as the listed ones.
export function useItemRef(ref: string | null, items: Item[]): Item | null {
  const listed = useMemo(
    () => (ref === null ? null : (items.find((item) => matchesItemRef(item, ref)) ?? null)),
    [ref, items],
  )
  const [fetched, setFetched] = useState<{ ref: string; item: Item | null } | null>(null)

  useEffect(() => {
    if (ref === null || listed !== null) return
    let cancelled = false
    fetchItem(ref)
      .then((item) => {
        if (!cancelled) setFetched({ ref, item })
      })
      .catch(() => {
        // Unreachable core: the connection banner already says so, and the
        // last fetched copy (if any) stays on screen.
      })
    return () => {
      cancelled = true
    }
  }, [ref, listed, items])

  if (ref === null) return null
  if (listed !== null) return listed
  return fetched?.ref === ref ? fetched.item : null
}
