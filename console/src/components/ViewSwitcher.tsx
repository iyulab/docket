interface ViewSwitcherProps {
  view: 'list' | 'board'
  onChange: (view: 'list' | 'board') => void
}

export function ViewSwitcher({ view, onChange }: ViewSwitcherProps) {
  return (
    <div className="view-switcher" role="tablist">
      <button
        type="button"
        role="tab"
        aria-selected={view === 'list'}
        className={view === 'list' ? 'view-switcher-btn active' : 'view-switcher-btn'}
        onClick={() => onChange('list')}
      >
        List
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={view === 'board'}
        className={view === 'board' ? 'view-switcher-btn active' : 'view-switcher-btn'}
        onClick={() => onChange('board')}
      >
        Board
      </button>
    </div>
  )
}
