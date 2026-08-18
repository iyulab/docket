import { act } from 'react-dom/test-utils'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it } from 'vitest'
import { Markdown } from './Markdown'

let container: HTMLDivElement | null = null

afterEach(() => {
  container?.remove()
  container = null
})

function renderMarkdown(text: string): HTMLDivElement {
  container = document.createElement('div')
  document.body.appendChild(container)
  const root = createRoot(container)
  act(() => {
    root.render(<Markdown text={text} />)
  })
  return container
}

describe('Markdown', () => {
  it('renders an inline image from markdown image syntax', () => {
    const el = renderMarkdown('![diagram](https://example.com/diagram.png)')
    const img = el.querySelector('img')
    expect(img).not.toBeNull()
    expect(img?.getAttribute('src')).toBe('https://example.com/diagram.png')
    expect(img?.getAttribute('alt')).toBe('diagram')
  })
})
