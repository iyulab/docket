import type { Item } from '../api'

interface ItemListProps {
  items: Item[]
  selectedId: string | null
  onSelect: (id: string) => void
}

// created_at/updated_at are Unix milliseconds (docket-core's now_millis()).
function formatRelativeTime(unixMillis: number): string {
  const diffMs = Date.now() - unixMillis
  const diffMin = Math.round(diffMs / 60000)
  if (diffMin < 1) return '방금 전'
  if (diffMin < 60) return `${diffMin}분 전`
  const diffHour = Math.round(diffMin / 60)
  if (diffHour < 24) return `${diffHour}시간 전`
  const diffDay = Math.round(diffHour / 24)
  return `${diffDay}일 전`
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
              {item.resolution && (
                <span className={`badge badge-${item.resolution}`}>{item.resolution}</span>
              )}
            </td>
            <td>
              {item.tags.map((tag) => (
                <span
                  key={tag}
                  className={
                    tag.startsWith('found-in:') ? 'tag-chip tag-chip-found-in' : 'tag-chip'
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
