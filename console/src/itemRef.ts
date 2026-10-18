import type { Item } from './api'

// An item is referred to either by its canonical `id` or by its short `seq`
// alias — `83` or `#83`, the form people write in comments and handoffs
// (ADR-0016). docket-core resolves both at `/items/{id}`; these helpers let the
// console agree with it wherever a reference arrives from a person: the
// `?item=` URL parameter and the search box.

const SEQ_REF = /^#?(\d+)$/

/** The `seq` a reference names, or `null` when it is not a seq reference. */
export function parseSeqRef(ref: string): number | null {
  const match = SEQ_REF.exec(ref.trim())
  return match ? Number(match[1]) : null
}

export function matchesItemRef(item: Item, ref: string): boolean {
  if (item.id === ref) return true
  const seq = parseSeqRef(ref)
  return seq !== null && item.seq === seq
}
