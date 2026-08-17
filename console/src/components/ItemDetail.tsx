import { useEffect, useState } from 'react'
import type { Comment, Item } from '../api'
import { fetchComments } from '../api'

interface ItemDetailProps {
  item: Item | null
  loading: boolean
  onClose: () => void
}

export function ItemDetail({ item, loading, onClose }: ItemDetailProps) {
  const [comments, setComments] = useState<Comment[]>([])
  const [commentsError, setCommentsError] = useState(false)

  // Depends on item?.id, not `item` itself — useItems() returns a new
  // array (and new item objects) on every 5s poll, so depending on the
  // whole object would refetch comments every tick even when the
  // selection hasn't changed.
  useEffect(() => {
    if (!item) {
      setComments([])
      setCommentsError(false)
      return
    }
    let cancelled = false
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
      {item.tags.length > 0 && (
        <div className="item-detail-tags">
          {item.tags.map((tag) => (
            <span
              key={tag}
              className={tag.startsWith('found-in:') ? 'tag-chip tag-chip-found-in' : 'tag-chip'}
            >
              {tag}
            </span>
          ))}
        </div>
      )}
      {item.body && <p className="item-detail-body">{item.body}</p>}
      <div className="item-detail-comments">
        <h3>댓글</h3>
        {commentsError && <p className="banner banner-error">댓글을 불러오지 못했습니다.</p>}
        {!commentsError && comments.length === 0 && <p>댓글이 없습니다.</p>}
        <ul>
          {comments.map((comment) => (
            <li key={comment.id}>
              <span className="comment-author">{comment.author}</span>
              <span className="comment-body">{comment.body}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
