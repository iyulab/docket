import { useItems } from './useItems'
import { Board } from './components/Board'

export default function App() {
  const { items, connected } = useItems()

  return (
    <div className="app">
      <header className="app-header">
        <h1>docket-console</h1>
        {!connected && (
          <div className="banner banner-error" role="status">
            Can&rsquo;t reach docket-core &mdash; showing last known state.
          </div>
        )}
      </header>
      <Board items={items} />
    </div>
  )
}
