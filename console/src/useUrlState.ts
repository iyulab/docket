import { useCallback, useEffect, useState } from 'react'

export function useUrlState<T>(
  key: string,
  parse: (raw: string | null) => T,
  serialize: (value: T) => string | null,
  // 'replace' (default) for lateral filter refinements — no history entry
  // per checkbox toggle. 'push' is for actual screen navigation (list <->
  // detail): without a history entry to land on, the browser's native Back
  // button has nothing of ours to step to and exits the app entirely
  // instead of returning to the list.
  navigation: 'replace' | 'push' = 'replace',
): [T, (value: T) => void] {
  const [value, setValue] = useState<T>(() =>
    parse(new URLSearchParams(window.location.search).get(key)),
  )

  const update = useCallback(
    (next: T) => {
      setValue(next)
      const params = new URLSearchParams(window.location.search)
      const serialized = serialize(next)
      if (serialized === null) {
        params.delete(key)
      } else {
        params.set(key, serialized)
      }
      const query = params.toString()
      const url = query ? `?${query}` : window.location.pathname
      if (navigation === 'push') {
        window.history.pushState(null, '', url)
      } else {
        window.history.replaceState(null, '', url)
      }
    },
    [key, serialize, navigation],
  )

  useEffect(() => {
    function onPopState() {
      setValue(parse(new URLSearchParams(window.location.search).get(key)))
    }
    window.addEventListener('popstate', onPopState)
    return () => window.removeEventListener('popstate', onPopState)
  }, [key, parse])

  return [value, update]
}
