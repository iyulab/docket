import type { Item } from '../api'
import { Card } from './Card'

export function Column({ title, items }: { title: string; items: Item[] }) {
  return (
    <div className="column">
      <div className="column-header">
        <span>{title}</span>
        <span className="column-count">{items.length}</span>
      </div>
      <div className="column-body">
        {items.map((item) => (
          <Card key={item.id} item={item} />
        ))}
      </div>
    </div>
  )
}
