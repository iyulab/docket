import type { Item } from '../api'
import { Card } from './Card'

interface ColumnProps {
  title: string
  items: Item[]
  selectedId: string | null
  onSelect: (id: string) => void
}

export function Column({ title, items, selectedId, onSelect }: ColumnProps) {
  return (
    <div className="column">
      <div className="column-header">
        <span>{title}</span>
        <span className="column-count">{items.length}</span>
      </div>
      <div className="column-body">
        {items.map((item) => (
          <Card key={item.id} item={item} selected={item.id === selectedId} onSelect={onSelect} />
        ))}
      </div>
    </div>
  )
}
