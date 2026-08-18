import { useMemo } from 'react'
import { marked } from 'marked'
import DOMPurify from 'dompurify'

marked.setOptions({ breaks: true, gfm: true })

// Item body/comment text is opaque caller-defined content (any docket-mcp
// worker can set it) — rendering it as HTML means sanitizing it exactly as
// carefully as any other untrusted-HTML surface, regardless of markdown
// syntax being unlikely to carry an attack on its own.
export function Markdown({ text, className }: { text: string; className?: string }) {
  const html = useMemo(() => {
    const parsed = marked.parse(text, { async: false })
    return DOMPurify.sanitize(parsed)
  }, [text])

  return <div className={className} dangerouslySetInnerHTML={{ __html: html }} />
}
