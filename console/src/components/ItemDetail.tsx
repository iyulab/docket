import { useEffect, useState } from 'react'
import type { Comment, Item } from '../api'
import { addItemTags, approveItem, claimItem, fetchComments, removeItemTags, submitItem } from '../api'
import { FOUND_IN_PREFIX } from '../filters'

// The console is a human looking at a browser tab, not a docket-mcp worker
// session — multi-user identity is out of scope while docket stays
// single-owner (ADR-0006), so claim/submit are attributed to one fixed id.
const CONSOLE_WORKER_ID = 'console'

interface ItemDetailProps {
  item: Item | null
  loading: boolean
  onClose: () => void
  /** Called after a write op succeeds, so the caller can re-poll immediately
   * instead of waiting for the next scheduled tick. */
  onMutated: () => void
}

export function ItemDetail({ item, loading, onClose, onMutated }: ItemDetailProps) {
  const [comments, setComments] = useState<Comment[]>([])
  const [commentsError, setCommentsError] = useState(false)
  const [actionPending, setActionPending] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)
  const [tagDraft, setTagDraft] = useState('')

  // Depends on item?.id, not `item` itself — useItems() returns a new
  // array (and new item objects) on every 5s poll, so depending on the
  // whole object would refetch comments every tick even when the
  // selection hasn't changed.
  useEffect(() => {
    setActionError(null)
    setTagDraft('')
    if (!item) {
      setComments([])
      setCommentsError(false)
      return
    }
    let cancelled = false
    setComments([])
    setCommentsError(false)
    fetchComments(item.id)
      .then((next) => {
        if (!cancelled) setComments(next)
      })
      .catch(() => {
        if (!cancelled) setCommentsError(true)
      })
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item?.id])

  if (!item) {
    return (
      <div className="item-detail item-detail-empty">
        {loading ? '불러오는 중...' : '아이템을 선택하세요.'}
      </div>
    )
  }

  async function runAction(action: () => Promise<unknown>) {
    setActionPending(true)
    setActionError(null)
    try {
      await action()
      onMutated()
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err))
    } finally {
      setActionPending(false)
    }
  }

  const addTag = () => {
    const tag = tagDraft.trim()
    if (!tag) return
    setTagDraft('')
    void runAction(() => addItemTags(item.id, [tag]))
  }

  return (
    <div className="item-detail">
      <div className="item-detail-header">
        <h2>{item.title}</h2>
        <button type="button" onClick={onClose} aria-label="닫기">
          ×
        </button>
      </div>
      <dl className="item-detail-meta">
        <dt>topic</dt>
        <dd>{item.topic}</dd>
        <dt>state</dt>
        <dd>{item.state}</dd>
        {item.resolution && (
          <>
            <dt>resolution</dt>
            <dd>{item.resolution}</dd>
          </>
        )}
        {item.owner && (
          <>
            <dt>owner</dt>
            <dd>{item.owner}</dd>
          </>
        )}
      </dl>
      <div className="item-detail-actions">
        {item.state === 'open' && (
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => claimItem(item.id, CONSOLE_WORKER_ID))}
          >
            Claim
          </button>
        )}
        {item.state === 'claimed' && item.owner === CONSOLE_WORKER_ID && (
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => submitItem(item.id, CONSOLE_WORKER_ID))}
          >
            Submit
          </button>
        )}
        {item.state === 'resolved' && (
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => approveItem(item.id))}
          >
            Approve
          </button>
        )}
      </div>
      {actionError && <p className="banner banner-error">{actionError}</p>}
      <div className="item-detail-tags">
        {item.tags.map((tag) => (
          <span
            key={tag}
            className={tag.startsWith(FOUND_IN_PREFIX) ? 'tag-chip tag-chip-found-in' : 'tag-chip'}
          >
            {tag}
            <button
              type="button"
              className="tag-chip-remove"
              aria-label={`${tag} 태그 삭제`}
              disabled={actionPending}
              onClick={() => void runAction(() => removeItemTags(item.id, [tag]))}
            >
              ×
            </button>
          </span>
        ))}
        <input
          type="text"
          list="available-tags"
          className="tag-add-input"
          value={tagDraft}
          placeholder="태그 추가"
          disabled={actionPending}
          onChange={(e) => setTagDraft(e.target.value)}
          onBlur={addTag}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              addTag()
            }
          }}
        />
      </div>
      {item.body && <p className="item-detail-body">{item.body}</p>}
      <div className="item-detail-comments">
        <h3>댓글</h3>
        {commentsError && <p className="banner banner-error">댓글을 불러오지 못했습니다.</p>}
        {!commentsError && comments.length === 0 && <p>댓글이 없습니다.</p>}
        {!commentsError && (
          <ul>
            {comments.map((comment) => (
              <li key={comment.id}>
                <span className="comment-author">{comment.author}</span>
                <span className="comment-body">{comment.body}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
