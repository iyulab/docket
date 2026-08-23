// The console has no login/session (single-owner admin UI, ADR-0006) — but
// once `author` became load-bearing for approve/reject (ADR-0019, matched
// against an item's `requester`), a fixed literal stops working for anyone
// approving/rejecting a requester-bearing item. This is the same
// "opaque, caller-defined string" treatment `requester`/`assignee` already
// get everywhere else: whoever is at the keyboard types in who they're
// acting as, unverified, persisted per-browser so they don't retype it
// every visit.
const ACTING_AS_KEY = 'docket-console-acting-as'
export const DEFAULT_ACTING_AS = 'console'

export function getActingAs(): string {
  const stored = window.localStorage.getItem(ACTING_AS_KEY)?.trim()
  return stored ? stored : DEFAULT_ACTING_AS
}

export function setActingAs(value: string): void {
  const trimmed = value.trim()
  if (trimmed && trimmed !== DEFAULT_ACTING_AS) {
    window.localStorage.setItem(ACTING_AS_KEY, trimmed)
  } else {
    window.localStorage.removeItem(ACTING_AS_KEY)
  }
}
