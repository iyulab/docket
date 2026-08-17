import { useEffect, useMemo, useState } from 'react'
import type { ItemState } from './api'
import { fetchTags } from './api'
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

function parseStates(raw: string | null): ItemState[] {
  return raw ? (raw.split(',') as ItemState[]) : ['open', 'claimed']
}
function serializeStates(value: ItemState[]): string | null {
  return value.length ? value.join(',') : null
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
  const { items, connected, loading } = useItems(query)
  const [availableTags, setAvailableTags] = useState<string[]>([])

  useEffect(() => {
    fetchTags()
      .then((tags) => setAvailableTags(tags.map((t) => t.tag)))
      .catch(() => setAvailableTags([]))
  }, [])

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
  )
  const [view, setView] = useUrlState<'list' | 'board'>('view', parseView, serializeView)

  const topics = useMemo(() => deriveTopics(items), [items])

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
        relation={relation}
        onRelationChange={setRelation}
        sortKey={sortKey}
        onSortKeyChange={setSortKey}
      />
      <ViewSwitcher view={view} onChange={setView} />
      <div className="app-body">
        {view === 'list' ? (
          <>
            <ItemList items={visibleItems} selectedId={selectedId} onSelect={setSelectedId} />
            <ItemDetail
              item={selectedItem}
              loading={loading}
              onClose={() => setSelectedId(null)}
            />
          </>
        ) : (
          <Board items={visibleItems} />
        )}
      </div>
    </div>
  )
}
