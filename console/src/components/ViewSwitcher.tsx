interface ViewSwitcherProps {
  view: 'list' | 'board'
  onChange: (view: 'list' | 'board') => void
}

export function ViewSwitcher({ view, onChange }: ViewSwitcherProps) {
  return (
    <div className="view-switcher" role="tablist">
      <button
        type="button"
        id="view-tab-list"
        role="tab"
        aria-selected={view === 'list'}
        aria-controls="view-panel"
        className={view === 'list' ? 'view-switcher-btn active' : 'view-switcher-btn'}
        onClick={() => onChange('list')}
      >
        List
      </button>
      <button
        type="button"
        id="view-tab-board"
        role="tab"
        aria-selected={view === 'board'}
        aria-controls="view-panel"
        className={view === 'board' ? 'view-switcher-btn active' : 'view-switcher-btn'}
        onClick={() => onChange('board')}
      >
        Board
      </button>
    </div>
  )
}
