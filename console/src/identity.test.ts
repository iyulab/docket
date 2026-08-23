import { afterEach, describe, expect, it } from 'vitest'
import { DEFAULT_ACTING_AS, getActingAs, setActingAs } from './identity'

afterEach(() => {
  window.localStorage.clear()
})

describe('getActingAs/setActingAs', () => {
  it('defaults to the console literal when nothing is stored', () => {
    expect(getActingAs()).toBe(DEFAULT_ACTING_AS)
  })

  it('round-trips a value that was set', () => {
    setActingAs('alice')
    expect(getActingAs()).toBe('alice')
  })

  it('trims whitespace on write and falls back to the default on a blank value', () => {
    setActingAs('  alice  ')
    expect(getActingAs()).toBe('alice')

    setActingAs('   ')
    expect(getActingAs()).toBe(DEFAULT_ACTING_AS)
  })

  it('setting the default literal explicitly does not leave a stray stored value', () => {
    setActingAs('alice')
    setActingAs(DEFAULT_ACTING_AS)
    expect(window.localStorage.getItem('docket-console-acting-as')).toBeNull()
    expect(getActingAs()).toBe(DEFAULT_ACTING_AS)
  })
})
