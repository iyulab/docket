import { useCallback, useEffect, useState } from 'react'

export function useUrlState<T>(
  key: string,
  parse: (raw: string | null) => T,
  serialize: (value: T) => string | null,
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
      window.history.replaceState(null, '', query ? `?${query}` : window.location.pathname)
    },
    [key, serialize],
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
