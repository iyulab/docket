import type { Item, ItemState } from './api'

export type SortKey = 'updated_at' | 'created_at' | 'state'
export type Relation = 'to' | 'from' | null
export type RelationFilter = 'to' | 'from' | 'all'

export interface Filters {
  states: ItemState[]
  tags: string[]
  perspectiveTopic: string | null
  relation: RelationFilter
}

export const FOUND_IN_PREFIX = 'found-in:'

// Mirrors docket-core's `domain::identity_eq` (ADR-0021): a topic, requester or
// assignee names the same thing whichever case it is written in. The console has
// to agree with the server here — if the server folds and the console does not,
// the same item is classified two different ways depending on who is asked.
//
// ASCII only, deliberately: `toLowerCase()` folds non-ASCII too, which would put
// the disagreement back in the other direction for an identity the server leaves
// byte-exact.
function foldAscii(value: string): string {
  return value.replace(/[A-Z]/g, (c) => c.toLowerCase())
}

export function identityEq(a: string, b: string): boolean {
  return foldAscii(a) === foldAscii(b)
}

// `item.requester` (ADR-0010/ADR-0011) is the current way to record which
// topic this item is for. `found-in:<repo>` is the legacy opaque tag the
// same relation used to be recorded as, before requester existed — items
// filed before the deprecation may still carry only the tag, so both are
// checked (requester first, since it's the maintained source now).
//
// The tag's topic portion is folded too. That is not core folding tags — core
// never interprets them (ADR-0021 "Not changed") — it is this console parsing a
// convention it already owns, and the value it parses out is an identity.
export function relationOf(item: Item, perspectiveTopic: string): Relation {
  if (identityEq(item.topic, perspectiveTopic)) return 'to'
  if (item.requester && identityEq(item.requester, perspectiveTopic)) return 'from'
  if (
    item.tags.some(
      (tag) =>
        tag.startsWith(FOUND_IN_PREFIX) &&
        identityEq(tag.slice(FOUND_IN_PREFIX.length), perspectiveTopic),
    )
  ) {
    return 'from'
  }
  return null
}

export function matchesFilters(item: Item, filters: Filters): boolean {
  if (filters.states.length > 0 && !filters.states.includes(item.state)) {
    return false
  }
  if (filters.tags.length > 0 && !filters.tags.every((tag) => item.tags.includes(tag))) {
    return false
  }
  if (filters.perspectiveTopic && filters.relation !== 'all') {
    if (relationOf(item, filters.perspectiveTopic) !== filters.relation) {
      return false
    }
  }
  return true
}

const STATE_ORDER: Record<ItemState, number> = { open: 0, claimed: 1, resolved: 2, closed: 3 }

const SORT_COMPARATORS: Record<SortKey, (a: Item, b: Item) => number> = {
  updated_at: (a, b) => b.updated_at - a.updated_at,
  created_at: (a, b) => b.created_at - a.created_at,
  state: (a, b) => STATE_ORDER[a.state] - STATE_ORDER[b.state],
}

export function sortItems(items: Item[], sortKey: SortKey): Item[] {
  return [...items].sort(SORT_COMPARATORS[sortKey])
}

// Deduplicates by identity, not by string (ADR-0021) — otherwise one identity
// whose spelling drifted appears as two entries in the picker, and choosing
// either one silently filters to a subset of its own items. The first spelling
// seen wins, matching how `register_worker` keeps the spelling already stored.
export function deriveTopics(items: Item[]): string[] {
  const topics = new Map<string, string>()
  const add = (topic: string) => {
    const key = foldAscii(topic)
    if (!topics.has(key)) topics.set(key, topic)
  }
  for (const item of items) {
    add(item.topic)
    if (item.requester) {
      add(item.requester)
    }
    for (const tag of item.tags) {
      if (tag.startsWith(FOUND_IN_PREFIX)) {
        add(tag.slice(FOUND_IN_PREFIX.length))
      }
    }
  }
  return [...topics.values()].sort()
}
