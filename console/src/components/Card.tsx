import type { Item, Resolution } from '../api'
import { formatRelativeTime, formatStandingFor } from '../time'

const RESOLUTION_LABEL: Record<Resolution, string> = {
  done: 'done',
  duplicate: 'duplicate',
  wontfix: "won't fix",
  invalid: 'invalid',
  blocked: 'blocked',
  deferred: 'deferred',
}

interface CardProps {
  item: Item
  selected: boolean
  onSelect: (id: string) => void
}

function standingFor(item: Item): string | null {
  return formatStandingFor(item.state_since)
}

export function Card({ item, selected, onSelect }: CardProps) {
  return (
    <div
      className={selected ? 'card card-selected' : 'card'}
      onClick={() => onSelect(item.id)}
    >
      <div className="card-title">{item.title}</div>
      <div className="card-topic">{item.topic}</div>
      <div className="card-id">#{item.seq}</div>
      <div className="card-updated">{formatRelativeTime(item.updated_at)}</div>
      {item.turn && (
        <span className={`badge badge-turn-${item.turn}`}>
          {item.turn === 'assignee' ? '→ assignee' : '→ requester'}
          {standingFor(item) && <span className="turn-age">{standingFor(item)}</span>}
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
