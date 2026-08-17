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

const FOUND_IN_PREFIX = 'found-in:'

// The org convention that records who discovered/filed an item against
// another topic (central CLAUDE.md §5) — an opaque caller-defined tag,
// not core vocabulary, but the only one with real production usage for
// this relation (docs' `topic:<X>` mention convention has zero usage —
// verified against `list_tags()`, see design doc §0).
export function relationOf(item: Item, perspectiveTopic: string): Relation {
  if (item.topic === perspectiveTopic) return 'to'
  if (item.tags.includes(`${FOUND_IN_PREFIX}${perspectiveTopic}`)) return 'from'
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

export function deriveTopics(items: Item[]): string[] {
  const topics = new Set<string>()
  for (const item of items) {
    topics.add(item.topic)
    for (const tag of item.tags) {
      if (tag.startsWith(FOUND_IN_PREFIX)) {
        topics.add(tag.slice(FOUND_IN_PREFIX.length))
      }
    }
  }
  return [...topics].sort()
}
