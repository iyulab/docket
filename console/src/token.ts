// The API token (ADR-0026). When docket-core runs with `DOCKET_TOKEN`, every
// API call must carry it; the console loads without one (its shell and assets
// stay open) and asks for it on the first `401`.
//
// Like `identity.ts` it lives in localStorage — per browser, never in the URL,
// so a shared board link cannot carry it. localStorage is readable by any
// script on this origin; that is acceptable for the single-owner console the
// token already protects, and a reason the console serves no third-party
// script.
const TOKEN_KEY = 'docket-console-token'

export function getToken(): string | null {
  try {
    const stored = window.localStorage.getItem(TOKEN_KEY)?.trim()
    return stored ? stored : null
  } catch {
    // Storage can be blocked (private window, site data off): behave as if
    // no token was saved rather than breaking the page.
    return null
  }
}

export function setToken(value: string): void {
  const trimmed = value.trim()
  try {
    if (trimmed) {
      window.localStorage.setItem(TOKEN_KEY, trimmed)
    } else {
      window.localStorage.removeItem(TOKEN_KEY)
    }
  } catch {
    // See getToken: nothing to persist to.
  }
}
