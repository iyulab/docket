import type { Item, Resolution } from '../api'

const RESOLUTION_LABEL: Record<Resolution, string> = {
  done: 'done',
  duplicate: 'duplicate',
  wontfix: "won't fix",
  invalid: 'invalid',
}

interface CardProps {
  item: Item
  selected: boolean
  onSelect: (id: string) => void
}

export function Card({ item, selected, onSelect }: CardProps) {
  return (
    <div
      className={selected ? 'card card-selected' : 'card'}
      onClick={() => onSelect(item.id)}
    >
      <div className="card-title">{item.title}</div>
      <div className="card-topic">{item.topic}</div>
      <div className="card-id">{item.id.slice(0, 8)}</div>
      {item.turn && (
        <span className={`badge badge-turn-${item.turn}`}>
          {item.turn === 'to' ? '→ to' : '→ from'}
        </span>
      )}
      {item.resolution && (
        <span className={`badge badge-${item.resolution}`}>
          {RESOLUTION_LABEL[item.resolution]}
        </span>
      )}
    </div>
  )
}
