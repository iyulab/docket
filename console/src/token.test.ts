import { afterEach, describe, expect, it } from 'vitest'
import { getToken, setToken } from './token'

afterEach(() => {
  window.localStorage.clear()
})

describe('token', () => {
  it('is null until one is saved', () => {
    expect(getToken()).toBeNull()
  })

  it('round-trips a saved token, trimmed', () => {
    setToken('  secret-token-value  ')
    expect(getToken()).toBe('secret-token-value')
  })

  it('clears the stored token when saved blank', () => {
    setToken('secret-token-value')
    setToken('   ')
    expect(getToken()).toBeNull()
  })
})
