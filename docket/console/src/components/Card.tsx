import type { Item, Resolution } from '../api'

const RESOLUTION_LABEL: Record<Resolution, string> = {
  done: 'done',
  duplicate: 'duplicate',
  wontfix: "won't fix",
  invalid: 'invalid',
}

export function Card({ item }: { item: Item }) {
  return (
    <div className="card">
      <div className="card-title">{item.title}</div>
      <div className="card-topic">{item.topic}</div>
      <div className="card-id">{item.id.slice(0, 8)}</div>
      {item.resolution && (
        <span className={`badge badge-${item.resolution}`}>
          {RESOLUTION_LABEL[item.resolution]}
        </span>
      )}
    </div>
  )
}
