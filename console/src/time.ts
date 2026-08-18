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
