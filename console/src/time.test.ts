import { describe, expect, it } from 'vitest'
import { formatStandingFor } from './time'

const DAY = 86400000

describe('formatStandingFor', () => {
  it('reports whole days once the item has stood for at least one', () => {
    expect(formatStandingFor(Date.now() - 15 * DAY - 1000)).toBe('15일째')
  })

  it('renders nothing under a day — the age is not the interesting fact yet', () => {
    expect(formatStandingFor(Date.now() - 3600000)).toBeNull()
  })

  it('renders nothing when the server did not say', () => {
    // `state_since: null` — the item's last transition predates the event
    // log. Showing "0일째" there would be a claim the server never made.
    expect(formatStandingFor(null)).toBeNull()
  })

  it('floors rather than rounds, so it never overstates the wait', () => {
    expect(formatStandingFor(Date.now() - (2 * DAY - 1000))).toBe('1일째')
  })
})
