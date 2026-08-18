import { useEffect, useRef, useState } from 'react'
import type { Comment, Item } from '../api'
import {
  addItemTags,
  approveItem,
  claimItem,
  fetchComments,
  forceCloseItem,
  mergeItem,
  removeItem,
  removeItemTags,
  submitItem,
} from '../api'
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
  // Tracks which item is currently selected so a mutation's response,
  // arriving after the user has already switched to a different item, does
  // not paint its error/pending state onto the wrong item.
  const selectedItemIdRef = useRef<string | null>(null)

  // Depends on item?.id, not `item` itself — useItems() returns a new
  // array (and new item objects) on every 5s poll, so depending on the
  // whole object would refetch comments every tick even when the
  // selection hasn't changed.
  useEffect(() => {
    selectedItemIdRef.current = item?.id ?? null
    setActionError(null)
    setActionPending(false)
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

  const runAction = async (action: () => Promise<unknown>) => {
    const issuedFor = item.id
    setActionPending(true)
    setActionError(null)
    try {
      await action()
      // Always refresh — a completed mutation should update the list even
      // if the user has since selected a different item.
      onMutated()
    } catch (err) {
      if (selectedItemIdRef.current === issuedFor) {
        setActionError(err instanceof Error ? err.message : String(err))
      }
    } finally {
      if (selectedItemIdRef.current === issuedFor) {
        setActionPending(false)
      }
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
        <dd>
          {item.state}
          {item.turn && (
            <span className={`badge badge-turn-${item.turn}`}>
              {item.turn === 'to' ? '→ to' : '→ from'}
            </span>
          )}
        </dd>
        {item.resolution && (
          <>
            <dt>resolution</dt>
            <dd>{item.resolution}</dd>
          </>
        )}
        {item.from && (
          <>
            <dt>from</dt>
            <dd>{item.from}</dd>
          </>
        )}
        {item.to && (
          <>
            <dt>to</dt>
            <dd>{item.to}</dd>
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
        {item.state === 'claimed' && item.to === CONSOLE_WORKER_ID && (
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
      {item.state !== 'closed' && (
        // Admin operations (architecture.md's admin-operation mapping) —
        // assignee-agnostic and valid from any non-closed state, unlike the
        // claim/submit/approve flow above. Kept visually separate since
        // these bypass the normal workflow rather than advance it.
        <div className="item-detail-actions item-detail-admin-actions">
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => removeItem(item.id))}
          >
            Remove
          </button>
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => mergeItem(item.id))}
          >
            Merge
          </button>
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => forceCloseItem(item.id))}
          >
            Force-close
          </button>
        </div>
      )}
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
