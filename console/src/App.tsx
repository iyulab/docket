import { useCallback, useEffect, useMemo, useState } from 'react'
import type { ItemState } from './api'
import { fetchTags, fetchTopics } from './api'
import { useItems } from './useItems'
import { useUrlState } from './useUrlState'
import { deriveTopics, matchesFilters, sortItems } from './filters'
import type { RelationFilter, SortKey } from './filters'
import { Board } from './components/Board'
import { FilterBar } from './components/FilterBar'
import { ItemList } from './components/ItemList'
import { ItemDetail } from './components/ItemDetail'
import { ViewSwitcher } from './components/ViewSwitcher'

function parseTopic(raw: string | null): string | null {
  return raw
}
function serializeTopic(value: string | null): string | null {
  return value
}

function parseQuery(raw: string | null): string {
  return raw ?? ''
}
function serializeQuery(value: string): string | null {
  const trimmed = value.trim()
  return trimmed ? trimmed : null
}

const VALID_STATES: ItemState[] = ['open', 'claimed', 'resolved', 'closed']

// `none` is a sentinel for "every checkbox unchecked" (no state filter,
// show everything) — distinct from the param being absent, which means the
// default (open/claimed) applies. Without it, an empty selection serializes
// to no param at all and a refresh silently reverts to the default.
function parseStates(raw: string | null): ItemState[] {
  if (raw === null) return ['open', 'claimed']
  if (raw === 'none') return []
  return raw.split(',').filter((s): s is ItemState => (VALID_STATES as string[]).includes(s))
}
function serializeStates(value: ItemState[]): string | null {
  return value.length ? value.join(',') : 'none'
}

// Mirrors the server's own default-excluded/archive-only toggle (ADR-0013/
// 0014) rather than a third `none`-style tri-state — an item is either in
// the default view or the archive browse, never both at once.
function parseArchived(raw: string | null): boolean {
  return raw === 'true'
}
function serializeArchived(value: boolean): string | null {
  return value ? 'true' : null
}

function parseTags(raw: string | null): string[] {
  return raw ? raw.split(',') : []
}
function serializeTags(value: string[]): string | null {
  return value.length ? value.join(',') : null
}

function parseRelation(raw: string | null): RelationFilter {
  return raw === 'to' || raw === 'from' ? raw : 'all'
}
function serializeRelation(value: RelationFilter): string | null {
  return value === 'all' ? null : value
}

function parseSortKey(raw: string | null): SortKey {
  return raw === 'created_at' || raw === 'state' ? raw : 'updated_at'
}
function serializeSortKey(value: SortKey): string | null {
  return value === 'updated_at' ? null : value
}

function parseSelectedId(raw: string | null): string | null {
  return raw
}
function serializeSelectedId(value: string | null): string | null {
  return value
}

function parseView(raw: string | null): 'list' | 'board' {
  return raw === 'board' ? 'board' : 'list'
}
function serializeView(value: 'list' | 'board'): string | null {
  return value === 'list' ? null : value
}

export default function App() {
  const [query, setQuery] = useUrlState<string>('q', parseQuery, serializeQuery)
  const [archived, setArchived] = useUrlState<boolean>(
    'archived',
    parseArchived,
    serializeArchived,
  )
  const { items, connected, loading, refresh } = useItems(query, archived)
  const [availableTags, setAvailableTags] = useState<string[]>([])
  const [serverTopics, setServerTopics] = useState<string[]>([])

  // A tag mutation (add_tags with a brand-new tag) can introduce a value
  // this list doesn't have yet — re-load whenever an item mutation
  // completes, not just once at mount, so the autocomplete doesn't go stale
  // until a full page reload.
  const loadTags = useCallback(() => {
    fetchTags()
      .then((tags) => setAvailableTags(tags.map((t) => t.tag)))
      .catch(() => setAvailableTags([]))
  }, [])

  // `list_topics` (ADR-0014) counts every non-archived item's `topic`
  // regardless of the console's own `limit`/`offset` page — a topic whose
  // items all fall outside the currently-fetched page would otherwise be
  // invisible to `deriveTopics(items)` below. Merged with, not swapped for,
  // the item-derived set: `deriveTopics` also surfaces `requester` and
  // `found-in:`-tagged identities (relationOf's "from" side, see
  // filters.ts), which aren't `item.topic` values and `list_topics` has no
  // way to know about.
  const loadTopics = useCallback(() => {
    fetchTopics()
      .then((topics) => setServerTopics(topics.map((t) => t.topic)))
      .catch(() => setServerTopics([]))
  }, [])

  useEffect(() => {
    loadTags()
    loadTopics()
  }, [loadTags, loadTopics])

  const [perspectiveTopic, setPerspectiveTopic] = useUrlState<string | null>(
    'topic',
    parseTopic,
    serializeTopic,
  )
  const [states, setStates] = useUrlState<ItemState[]>('state', parseStates, serializeStates)
  const [tags, setTags] = useUrlState<string[]>('tag', parseTags, serializeTags)
  const [relation, setRelation] = useUrlState<RelationFilter>(
    'relation',
    parseRelation,
    serializeRelation,
  )
  const [sortKey, setSortKey] = useUrlState<SortKey>('sort', parseSortKey, serializeSortKey)
  const [selectedId, setSelectedId] = useUrlState<string | null>(
    'item',
    parseSelectedId,
    serializeSelectedId,
    'push',
  )
  const [view, setView] = useUrlState<'list' | 'board'>('view', parseView, serializeView)

  const topics = useMemo(
    () => [...new Set([...serverTopics, ...deriveTopics(items)])].sort(),
    [serverTopics, items],
  )

  const filters = useMemo(
    () => ({ states, tags, perspectiveTopic, relation }),
    [states, tags, perspectiveTopic, relation],
  )

  const visibleItems = useMemo(
    () => sortItems(items.filter((item) => matchesFilters(item, filters)), sortKey),
    [items, filters, sortKey],
  )

  const selectedItem = useMemo(
    () => items.find((item) => item.id === selectedId) ?? null,
    [items, selectedId],
  )

  return (
    <div className="app">
      <header className="app-header">
        <h1>docket-console</h1>
        {!connected && (
          <div className="banner banner-error" role="status">
            Can&rsquo;t reach docket-core &mdash; showing last known state.
          </div>
        )}
      </header>
      {selectedId ? (
        <ItemDetail
          item={selectedItem}
          loading={loading}
          onBack={() => setSelectedId(null)}
          onMutated={() => {
            refresh()
            loadTags()
            loadTopics()
          }}
        />
      ) : (
        <>
          <FilterBar
            query={query}
            onQueryChange={setQuery}
            topics={topics}
            perspectiveTopic={perspectiveTopic}
            onPerspectiveTopicChange={setPerspectiveTopic}
            states={states}
            onStatesChange={setStates}
            tags={tags}
            onTagsChange={setTags}
            availableTags={availableTags}
            archived={archived}
            onArchivedChange={setArchived}
            relation={relation}
            onRelationChange={setRelation}
            sortKey={sortKey}
            onSortKeyChange={setSortKey}
          />
          <ViewSwitcher view={view} onChange={setView} />
          <div
            id="view-panel"
            role="tabpanel"
            aria-labelledby={view === 'list' ? 'view-tab-list' : 'view-tab-board'}
            className="app-body"
          >
            {view === 'list' ? (
              <ItemList items={visibleItems} selectedId={selectedId} onSelect={setSelectedId} />
            ) : (
              <Board items={visibleItems} selectedId={selectedId} onSelect={setSelectedId} />
            )}
          </div>
        </>
      )}
    </div>
  )
}
