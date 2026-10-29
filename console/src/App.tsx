import { useCallback, useEffect, useMemo, useState } from 'react'
import type { ItemState } from './api'
import { fetchTags, fetchTopics } from './api'
import { getActingAs, setActingAs } from './identity'
import { setToken } from './token'
import { useItems } from './useItems'
import { useUrlState } from './useUrlState'
import { useItemRef } from './useItemRef'
import { parseSeqRef } from './itemRef'
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
// default applies. Without it, an empty selection serializes to no param at
// all and a refresh silently reverts to the default.
//
// The default is every state that still needs someone: `resolved` included,
// because a resolved item is waiting on its requester's decision — the row a
// person most often has to act on (approve, or force-approve when no
// requester session will). Only `closed` is done.
const DEFAULT_STATES: ItemState[] = ['open', 'claimed', 'resolved']

function parseStates(raw: string | null): ItemState[] {
  if (raw === null) return DEFAULT_STATES
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

// Not a `useUrlState` field — this identifies the person at the keyboard,
// not the board's filter/view state, so it lives in localStorage (per
// browser, not per shared link) via `identity.ts`.
function ActingAs() {
  const [value, setValue] = useState(getActingAs)

  return (
    <label className="acting-as" title="approve/reject/admin 액션에 author로 기록되는 값 — requester와 일치해야 approve/reject가 통과함(ADR-0019)">
      acting as
      <input
        type="text"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => {
          setActingAs(value)
          setValue(getActingAs())
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter') e.currentTarget.blur()
        }}
      />
    </label>
  )
}

// Shown when docket-core runs with `DOCKET_TOKEN` and the console has no valid
// token yet (ADR-0026). Saving it re-polls straight away.
function TokenPrompt({ onSaved }: { onSaved: () => void }) {
  const [value, setValue] = useState('')

  return (
    <form
      className="banner banner-error token-prompt"
      onSubmit={(e) => {
        e.preventDefault()
        setToken(value)
        setValue('')
        onSaved()
      }}
    >
      <label>
        docket-core requires a token or password
        <input
          type="password"
          autoComplete="off"
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
      </label>
      <button type="submit" disabled={!value.trim()}>
        Save
      </button>
    </form>
  )
}

export default function App() {
  const [query, setQuery] = useUrlState<string>('q', parseQuery, serializeQuery)
  const [archived, setArchived] = useUrlState<boolean>(
    'archived',
    parseArchived,
    serializeArchived,
  )
  const { items, connected, unauthorized, loading, refresh } = useItems(query, archived)
  const [availableTags, setAvailableTags] = useState<string[]>([])
  const [serverTopics, setServerTopics] = useState<string[]>([])
  // Declared spellings that fold into each canonical topic (ADR-0022) — kept
  // separate from `serverTopics` because `deriveTopics(items)` below adds
  // topic-like identities `list_topics` never returns, which have no alias
  // list to look up. Empty for a topic with no aliases, same as the API.
  const [topicAliases, setTopicAliases] = useState<Record<string, string[]>>({})

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
      .then((topics) => {
        setServerTopics(topics.map((t) => t.topic))
        setTopicAliases(Object.fromEntries(topics.map((t) => [t.topic, t.aliases ?? []])))
      })
      .catch(() => {
        setServerTopics([])
        setTopicAliases({})
      })
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

  // A search for `83` or `#83` is someone who knows the item's number: that
  // item goes to the top, whatever the filters say, above the full-text
  // matches the same query still returns.
  const searchedSeq = parseSeqRef(query)
  const searchedItem = useItemRef(searchedSeq === null ? null : String(searchedSeq), items)

  const visibleItems = useMemo(() => {
    const matching = sortItems(
      items.filter((item) => matchesFilters(item, filters)),
      sortKey,
    )
    if (!searchedItem) return matching
    return [searchedItem, ...matching.filter((item) => item.id !== searchedItem.id)]
  }, [items, filters, sortKey, searchedItem])

  // `?item=` takes an id or a seq, so `/?item=83` (where docket-core sends a
  // browser opening `/items/83`) works as a link.
  const selectedItem = useItemRef(selectedId, items)

  return (
    <div className="app">
      <header className="app-header">
        <h1>docket-console</h1>
        <ActingAs />
        {unauthorized && <TokenPrompt onSaved={refresh} />}
        {!connected && !unauthorized && (
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
            topicAliases={topicAliases}
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
