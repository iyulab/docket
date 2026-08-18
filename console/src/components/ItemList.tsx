import type { Item } from '../api'
import { FOUND_IN_PREFIX } from '../filters'
import { formatRelativeTime } from '../time'

interface ItemListProps {
  items: Item[]
  selectedId: string | null
  onSelect: (id: string) => void
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
          <th>topic</th>
          <th>state</th>
          <th>tags</th>
          <th>갱신</th>
        </tr>
      </thead>
      <tbody>
        {items.map((item) => (
          <tr
            key={item.id}
            className={item.id === selectedId ? 'item-row item-row-selected' : 'item-row'}
            onClick={() => onSelect(item.id)}
          >
            <td>{item.title}</td>
            <td>{item.topic}</td>
            <td>
              <span className={`badge badge-state-${item.state}`}>{item.state}</span>
              {item.turn && (
                <span className={`badge badge-turn-${item.turn}`}>
                  {item.turn === 'to' ? '→ to' : '→ from'}
                </span>
              )}
              {item.resolution && (
                <span className={`badge badge-${item.resolution}`}>{item.resolution}</span>
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
      </tbody>
    </table>
  )
}
