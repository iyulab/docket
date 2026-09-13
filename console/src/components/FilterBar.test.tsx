import { act } from 'react-dom/test-utils'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it } from 'vitest'
import { FilterBar } from './FilterBar'

let container: HTMLDivElement | null = null

afterEach(() => {
  container?.remove()
  container = null
})

const noop = () => {}

function renderFilterBar(topics: string[], topicAliases: Record<string, string[]>) {
  container = document.createElement('div')
  document.body.appendChild(container)
  const root = createRoot(container)
  act(() => {
    root.render(
      <FilterBar
        query=""
        onQueryChange={noop}
        topics={topics}
        topicAliases={topicAliases}
        perspectiveTopic={null}
        onPerspectiveTopicChange={noop}
        states={[]}
        onStatesChange={noop}
        tags={[]}
        onTagsChange={noop}
        availableTags={[]}
        archived={false}
        onArchivedChange={noop}
        relation="all"
        onRelationChange={noop}
        sortKey="updated_at"
        onSortKeyChange={noop}
      />,
    )
  })
  return container
}

// `list_topics` has returned `aliases` for a while, but the topic dropdown
// rendered only the canonical spelling — a caller declaring the wrong
// alias had no way to notice from this screen. The dropdown is already
// every topic's natural gathering point, so folding the declared
// spellings in here needs no new UI surface.
describe('FilterBar topic dropdown', () => {
  it('shows declared aliases inline next to the canonical topic', () => {
    const el = renderFilterBar(
      ['acme/widget', 'acme/gadget'],
      { 'acme/widget': ['widget'] },
    )
    const options = Array.from(el.querySelectorAll('select')[0].querySelectorAll('option'))
    const labels = options.map((o) => o.textContent)
    expect(labels).toContain('acme/widget (aka: widget)')
    expect(labels).toContain('acme/gadget')
  })

  it('renders the plain topic name when it has no declared aliases', () => {
    const el = renderFilterBar(['acme/gadget'], {})
    const options = Array.from(el.querySelectorAll('select')[0].querySelectorAll('option'))
    const labels = options.map((o) => o.textContent)
    expect(labels).toContain('acme/gadget')
    expect(labels.some((l) => l?.includes('aka'))).toBe(false)
  })
})
