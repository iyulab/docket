// created_at/updated_at are Unix milliseconds (docket-core's now_millis()).
export function formatRelativeTime(unixMillis: number): string {
  const diffMs = Date.now() - unixMillis
  const diffMin = Math.round(diffMs / 60000)
  if (diffMin < 1) return '방금 전'
  if (diffMin < 60) return `${diffMin}분 전`
  const diffHour = Math.round(diffMin / 60)
  if (diffHour < 24) return `${diffHour}시간 전`
  const diffDay = Math.round(diffHour / 24)
  return `${diffDay}일 전`
}

// How long the item has stood where it stands (ADR-0024's `state_since`),
// rendered next to the turn badge because that is the claim it qualifies:
// "→ requester" says whose move it is, this says since when. Deliberately
// separate from formatRelativeTime — that one reads `updated_at`, which a
// comment resets, so an item standing for two weeks can read "1시간 전"
// there while nothing has actually moved.
//
// `null` (nothing to render) in two cases: the server didn't say (the
// item's last transition predates the event log), and anything under a
// day, where the age isn't the interesting fact about the row.
export function formatStandingFor(stateSince: number | null): string | null {
  if (stateSince == null) return null
  const days = Math.floor((Date.now() - stateSince) / 86400000)
  return days < 1 ? null : `${days}일째`
}
