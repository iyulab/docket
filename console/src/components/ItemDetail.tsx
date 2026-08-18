import { useEffect, useRef, useState } from 'react'
import type { Comment, Item } from '../api'
import {
  addItemTags,
  approveItem,
  assigneeDisplay,
  claimItem,
  fetchComments,
  forceCloseItem,
  mergeItem,
  removeItem,
  removeItemTags,
  submitItem,
} from '../api'
import { FOUND_IN_PREFIX } from '../filters'
import { formatRelativeTime } from '../time'
import { Markdown } from './Markdown'

// The console is a human looking at a browser tab, not a docket-mcp worker
// session — multi-user identity is out of scope while docket stays
// single-owner (ADR-0006), so claim/submit are attributed to one fixed id.
const CONSOLE_WORKER_ID = 'console'

interface ItemDetailProps {
  item: Item | null
  loading: boolean
  /** Navigates back to the list/board page. */
  onBack: () => void
  /** Called after a write op succeeds, so the caller can re-poll immediately
   * instead of waiting for the next scheduled tick. */
  onMutated: () => void
}

export function ItemDetail({ item, loading, onBack, onMutated }: ItemDetailProps) {
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

  const backButton = (
    <button type="button" className="item-page-back" onClick={onBack}>
      ← 목록으로
    </button>
  )

  if (!item) {
    return (
      <div className="item-page">
        {backButton}
        <div className="item-page-empty">
          {loading ? '불러오는 중...' : '해당 아이템을 찾을 수 없습니다.'}
        </div>
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
    <div className="item-page">
      {backButton}
      <div className="item-page-header">
        <h1>{item.title}</h1>
      </div>
      <dl className="item-page-meta">
        <dt>topic</dt>
        <dd>{item.topic}</dd>
        <dt>state</dt>
        <dd>
          {item.state}
          {item.turn && (
            <span className={`badge badge-turn-${item.turn}`}>
              {item.turn === 'assignee' ? '→ assignee' : '→ requester'}
            </span>
          )}
        </dd>
        {item.resolution && (
          <>
            <dt>resolution</dt>
            <dd>{item.resolution}</dd>
          </>
        )}
        {item.requester && (
          <>
            <dt>requester</dt>
            <dd>{item.requester}</dd>
          </>
        )}
        <dt>assignee</dt>
        <dd>
          {(() => {
            const assignee = assigneeDisplay(item)
            return (
              <span className={assignee.isFallback ? 'item-assignee-fallback' : undefined}>
                {assignee.value}
              </span>
            )
          })()}
        </dd>
      </dl>
      <div className="item-page-actions">
        {item.state === 'open' && (
          <button
            type="button"
            disabled={actionPending}
            onClick={() => void runAction(() => claimItem(item.id, CONSOLE_WORKER_ID))}
          >
            Claim
          </button>
        )}
        {item.state === 'claimed' && item.assignee === CONSOLE_WORKER_ID && (
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
        <div className="item-page-actions item-page-admin-actions">
          <button
            type="button"
            title="상태를 invalid로 종료 — 실수로 만든 항목"
            disabled={actionPending}
            onClick={() => void runAction(() => removeItem(item.id))}
          >
            Remove
          </button>
          <button
            type="button"
            title="상태를 duplicate로 종료 — 다른 항목의 중복"
            disabled={actionPending}
            onClick={() => void runAction(() => mergeItem(item.id))}
          >
            Merge
          </button>
          <button
            type="button"
            title="상태를 wontfix로 종료 — 더 이상 무관"
            disabled={actionPending}
            onClick={() => void runAction(() => forceCloseItem(item.id))}
          >
            Force-close
          </button>
        </div>
      )}
      {actionError && <p className="banner banner-error">{actionError}</p>}
      <div className="item-page-tags">
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
      {item.body && <Markdown className="item-page-body" text={item.body} />}
      <div className="item-page-comments">
        <h2>댓글</h2>
        {commentsError && <p className="banner banner-error">댓글을 불러오지 못했습니다.</p>}
        {!commentsError && comments.length === 0 && <p>댓글이 없습니다.</p>}
        {!commentsError && (
          <ul>
            {comments.map((comment) => (
              <li key={comment.id}>
                <div className="comment-meta">
                  <span className="comment-author">{comment.author}</span>
                  <span className="comment-time">{formatRelativeTime(comment.created_at)}</span>
                </div>
                <Markdown className="comment-body" text={comment.body} />
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
