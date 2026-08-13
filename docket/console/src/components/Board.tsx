import type { Item, ItemState } from '../api'
import { Column } from './Column'

// Fixed column order — not derived from the data, so an empty column
// (e.g. no closed items yet) still renders in its place.
const COLUMNS: { state: ItemState; title: string }[] = [
  { state: 'open', title: 'Open' },
  { state: 'claimed', title: 'Claimed' },
  { state: 'resolved', title: 'Resolved' },
  { state: 'closed', title: 'Closed' },
]

export function Board({ items }: { items: Item[] }) {
  return (
    <div className="board">
      {COLUMNS.map(({ state, title }) => (
        <Column
          key={state}
          title={title}
          items={items.filter((item) => item.state === state)}
        />
      ))}
    </div>
  )
}
