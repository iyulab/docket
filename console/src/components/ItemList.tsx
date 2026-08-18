import { Fragment } from 'react'
import type { Item } from '../api'
import { toDisplay } from '../api'
import { FOUND_IN_PREFIX } from '../filters'
import { formatRelativeTime } from '../time'

interface ItemListProps {
  items: Item[]
  selectedId: string | null
  onSelect: (id: string) => void
}

// `topic` as its own column repeated the same string down an entire group of
// rows, and duplicated what `to`'s topic fallback (see `toDisplay`) already
// shows for unclaimed items. Grouping by topic instead — a header row per
// topic, once — carries the same information without the repetition, and
// reads closer to how the board's per-state columns already categorize.
function groupByTopic(items: Item[]): [string, Item[]][] {
  const order: string[] = []
  const groups = new Map<string, Item[]>()
  for (const item of items) {
    let group = groups.get(item.topic)
    if (!group) {
      group = []
      groups.set(item.topic, group)
      order.push(item.topic)
    }
    group.push(item)
  }
  return order.sort((a, b) => a.localeCompare(b)).map((topic) => [topic, groups.get(topic)!])
}

export function ItemList({ items, selectedId, onSelect }: ItemListProps) {
  if (items.length === 0) {
    return <p className="item-list-empty">조건에 맞는 아이템이 없습니다.</p>
  }

  return (
    <table className="item-list">
      <thead>
        <tr>
          <th>제목</th>
          <th>state</th>
          <th>from</th>
          <th>to</th>
          <th>turn</th>
          <th>tags</th>
          <th>갱신</th>
        </tr>
      </thead>
      <tbody>
        {groupByTopic(items).map(([topic, groupItems]) => (
          <Fragment key={topic}>
            <tr className="item-list-group-header">
              <th colSpan={7}>
                {topic} <span className="item-list-group-count">{groupItems.length}</span>
              </th>
            </tr>
            {groupItems.map((item) => (
              <tr
                key={item.id}
                className={item.id === selectedId ? 'item-row item-row-selected' : 'item-row'}
                onClick={() => onSelect(item.id)}
              >
                <td>{item.title}</td>
                <td>
                  <span className={`badge badge-state-${item.state}`}>{item.state}</span>
                  {item.resolution && (
                    <span className={`badge badge-${item.resolution}`}>{item.resolution}</span>
                  )}
                </td>
                <td className="item-list-worker-cell">{item.from ?? '—'}</td>
                <td className="item-list-worker-cell">
                  {(() => {
                    const to = toDisplay(item)
                    return (
                      <span className={to.isFallback ? 'item-to-fallback' : undefined}>
                        {to.value}
                      </span>
                    )
                  })()}
                </td>
                <td>
                  {item.turn ? (
                    <span className={`badge badge-turn-${item.turn}`}>
                      {item.turn === 'to' ? '→ to' : '→ from'}
                    </span>
                  ) : (
                    '—'
                  )}
                </td>
                <td>
                  {item.tags.map((tag) => (
                    <span
                      key={tag}
                      className={
                        tag.startsWith(FOUND_IN_PREFIX) ? 'tag-chip tag-chip-found-in' : 'tag-chip'
                      }
                    >
                      {tag}
                    </span>
                  ))}
                </td>
                <td>{formatRelativeTime(item.updated_at)}</td>
              </tr>
            ))}
          </Fragment>
        ))}
      </tbody>
    </table>
  )
}
