import { useEffect, useState } from 'react'
import type { ItemState } from '../api'
import type { RelationFilter, SortKey } from '../filters'

const STATES: ItemState[] = ['open', 'claimed', 'resolved', 'closed']
const SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: 'updated_at', label: '최근 갱신순' },
  { key: 'created_at', label: '생성일순' },
  { key: 'state', label: '상태순' },
]

interface FilterBarProps {
  query: string
  onQueryChange: (query: string) => void
  topics: string[]
  perspectiveTopic: string | null
  onPerspectiveTopicChange: (topic: string | null) => void
  states: ItemState[]
  onStatesChange: (states: ItemState[]) => void
  tags: string[]
  onTagsChange: (tags: string[]) => void
  availableTags: string[]
  archived: boolean
  onArchivedChange: (archived: boolean) => void
  relation: RelationFilter
  onRelationChange: (relation: RelationFilter) => void
  sortKey: SortKey
  onSortKeyChange: (key: SortKey) => void
}

function TagFilterInput({
  tags,
  onChange,
  availableTags,
}: {
  tags: string[]
  onChange: (tags: string[]) => void
  availableTags: string[]
}) {
  const [draft, setDraft] = useState(tags.join(', '))

  useEffect(() => {
    setDraft(tags.join(', '))
  }, [tags])

  function commit() {
    const next = draft
      .split(',')
      .map((t) => t.trim())
      .filter(Boolean)
    onChange(next)
  }

  return (
    <>
      <input
        type="text"
        list="available-tags"
        value={draft}
        placeholder="쉼표로 구분"
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            commit()
          }
        }}
      />
      <datalist id="available-tags">
        {availableTags.map((tag) => (
          <option key={tag} value={tag} />
        ))}
      </datalist>
    </>
  )
}

function QueryInput({ query, onChange }: { query: string; onChange: (query: string) => void }) {
  const [draft, setDraft] = useState(query)

  useEffect(() => {
    setDraft(query)
  }, [query])

  function commit() {
    onChange(draft.trim())
  }

  return (
    <input
      type="text"
      value={draft}
      placeholder="제목·본문·댓글 검색 (Enter로 적용)"
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault()
          commit()
        }
      }}
    />
  )
}

export function FilterBar({
  query,
  onQueryChange,
  topics,
  perspectiveTopic,
  onPerspectiveTopicChange,
  states,
  onStatesChange,
  tags,
  onTagsChange,
  availableTags,
  archived,
  onArchivedChange,
  relation,
  onRelationChange,
  sortKey,
  onSortKeyChange,
}: FilterBarProps) {
  function toggleState(state: ItemState) {
    onStatesChange(
      states.includes(state) ? states.filter((s) => s !== state) : [...states, state],
    )
  }

  return (
    <div className="filter-bar">
      <label className="filter-field">
        검색
        <QueryInput query={query} onChange={onQueryChange} />
      </label>

      <label className="filter-field">
        관점
        <select
          value={perspectiveTopic ?? ''}
          onChange={(e) => onPerspectiveTopicChange(e.target.value || null)}
        >
          <option value="">전체</option>
          {topics.map((topic) => (
            <option key={topic} value={topic}>
              {topic}
            </option>
          ))}
        </select>
      </label>

      <fieldset className="filter-field filter-states">
        <legend>state</legend>
        {STATES.map((state) => (
          <label key={state}>
            <input
              type="checkbox"
              checked={states.includes(state)}
              onChange={() => toggleState(state)}
            />
            {state}
          </label>
        ))}
      </fieldset>

      <label className="filter-field">
        tag
        <TagFilterInput tags={tags} onChange={onTagsChange} availableTags={availableTags} />
      </label>

      <label className="filter-field filter-archived" title="보관된 항목만 보기 — 기본 목록에서는 항상 제외됨">
        <input
          type="checkbox"
          checked={archived}
          onChange={(e) => onArchivedChange(e.target.checked)}
        />
        archived만 보기
      </label>

      <label className="filter-field">
        관점 필터
        <select
          value={relation}
          disabled={!perspectiveTopic}
          onChange={(e) => onRelationChange(e.target.value as RelationFilter)}
        >
          <option value="all">전체</option>
          <option value="to">to (내가 처리해야 함)</option>
          <option value="from">from (나를 막고 있음)</option>
        </select>
      </label>

      <label className="filter-field">
        정렬
        <select value={sortKey} onChange={(e) => onSortKeyChange(e.target.value as SortKey)}>
          {SORT_OPTIONS.map((opt) => (
            <option key={opt.key} value={opt.key}>
              {opt.label}
            </option>
          ))}
        </select>
      </label>
    </div>
  )
}
