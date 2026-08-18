use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{Comment, Item, Resolution, State, TagCount, TagMatch, Worker};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    /// The requested transition is not legal from the item's current state
    /// (e.g. claiming an already-claimed item, or an owner mismatch).
    #[error("conflict: {0}")]
    Conflict(String),
    /// The request is well-formed JSON but its values are unusable (e.g. a
    /// blank `topic`/`title`) — distinct from `Conflict`, which is about
    /// state, not input shape.
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}

/// A single connection guarded by a mutex. `claim`/`submit`/`approve` each
/// run one conditional `UPDATE ... WHERE state = ?` and check
/// `rows_affected`, so serializing access here is sufficient to make claim
/// exclusive: the loser's `UPDATE` matches zero rows because the winner's
/// write has already moved the row out of the expected state.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workers (
                id TEXT PRIMARY KEY,
                topics TEXT NOT NULL,
                online INTEGER NOT NULL,
                registered_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS items (
                id TEXT PRIMARY KEY,
                topic TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT,
                state TEXT NOT NULL,
                resolution TEXT,
                requester TEXT,
                assignee TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_items_topic ON items(topic);
            CREATE INDEX IF NOT EXISTS idx_items_state ON items(state);
            CREATE TABLE IF NOT EXISTS item_tags (
                item_id TEXT NOT NULL REFERENCES items(id),
                tag     TEXT NOT NULL,
                PRIMARY KEY (item_id, tag)
            );
            CREATE INDEX IF NOT EXISTS idx_item_tags_tag ON item_tags(tag);
            CREATE TABLE IF NOT EXISTS item_comments (
                id         TEXT PRIMARY KEY,
                item_id    TEXT NOT NULL REFERENCES items(id),
                author     TEXT NOT NULL,
                body       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_item_comments_item ON item_comments(item_id, created_at);
            CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
                title, body, content='items', content_rowid='rowid'
            );
            CREATE TRIGGER IF NOT EXISTS items_fts_ai AFTER INSERT ON items BEGIN
                INSERT INTO items_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
            END;
            CREATE TRIGGER IF NOT EXISTS items_fts_ad AFTER DELETE ON items BEGIN
                INSERT INTO items_fts(items_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
            END;
            CREATE TRIGGER IF NOT EXISTS items_fts_au AFTER UPDATE ON items BEGIN
                INSERT INTO items_fts(items_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
                INSERT INTO items_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
            END;
            CREATE VIRTUAL TABLE IF NOT EXISTS comments_fts USING fts5(
                body, content='item_comments', content_rowid='rowid'
            );
            CREATE TRIGGER IF NOT EXISTS comments_fts_ai AFTER INSERT ON item_comments BEGIN
                INSERT INTO comments_fts(rowid, body) VALUES (new.rowid, new.body);
            END;",
        )?;
        migrate_owner_to_requester_assignee(&conn)?;
        // Resyncs the external-content index against `items` on every open:
        // rows written before this virtual table existed are never covered by
        // the AFTER INSERT trigger, and an un-indexed row makes the AFTER
        // UPDATE trigger's 'delete' command fail the whole write. A row-diffing
        // backfill can't find those rows — `SELECT rowid FROM items_fts` reads
        // through to `items`, so it always reports every row as present.
        // 'rebuild' is idempotent and cheap at this project's scale.
        conn.execute("INSERT INTO items_fts(items_fts) VALUES('rebuild')", [])?;
        // Same rationale as items_fts above, applied to comments_fts. No
        // AFTER DELETE/UPDATE trigger exists for comments_fts because
        // item_comments is append-only (no edit/delete API, ADR-0009) — an
        // INSERT-only trigger can never fall out of sync with a table that
        // never changes existing rows.
        conn.execute(
            "INSERT INTO comments_fts(comments_fts) VALUES('rebuild')",
            [],
        )?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    pub fn register_worker(&self, id: &str, topics: &[String]) -> Result<Worker> {
        let topics_json = serde_json::to_string(topics).expect("Vec<String> always serializes");
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO workers (id, topics, online, registered_at) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(id) DO UPDATE SET topics = excluded.topics, online = 1",
            params![id, topics_json, now_millis()],
        )?;
        Ok(Worker {
            id: id.to_string(),
            topics: topics.to_vec(),
            online: true,
        })
    }

    pub fn create_item(
        &self,
        topic: &str,
        title: &str,
        body: Option<&str>,
        tags: &[String],
        requester: Option<&str>,
    ) -> Result<Item> {
        let topic = topic.trim();
        let title = title.trim();
        if topic.is_empty() {
            return Err(StoreError::Validation(
                "topic must not be blank".to_string(),
            ));
        }
        if title.is_empty() {
            return Err(StoreError::Validation(
                "title must not be blank".to_string(),
            ));
        }
        let requester = requester.map(str::trim).filter(|s| !s.is_empty());
        let id = Uuid::new_v4().to_string();
        let now = now_millis();
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO items (id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'open', NULL, ?5, NULL, ?6, ?6)",
            params![id, topic, title, body, requester, now],
        )?;
        for tag in tags {
            tx.execute(
                "INSERT INTO item_tags (item_id, tag) VALUES (?1, ?2)",
                params![id, tag],
            )?;
        }
        tx.commit()?;
        Ok(Item {
            id,
            topic: topic.to_string(),
            title: title.to_string(),
            body: body.map(str::to_string),
            state: State::Open,
            resolution: None,
            requester: requester.map(str::to_string),
            assignee: None,
            turn: Item::turn_for(State::Open),
            tags: tags.to_vec(),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn get_item(&self, id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Sets `from` (the requester) on an item that already exists — the one
    /// way to give an item a requester after creation, for items filed
    /// before ADR-0010 or before a requester identity was available.
    /// State-independent (works on a closed item too — this corrects
    /// metadata, it isn't a workflow transition).
    pub fn set_item_from(&self, id: &str, from: &str) -> Result<Item> {
        let from = from.trim();
        if from.is_empty() {
            return Err(StoreError::Validation("from must not be blank".to_string()));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let affected = conn.execute(
            "UPDATE items SET requester = ?1, updated_at = ?2 WHERE id = ?3",
            params![from, now_millis(), id],
        )?;
        if affected == 0 {
            return Err(StoreError::NotFound);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    pub fn get_worker(&self, id: &str) -> Result<Worker> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT id, topics, online FROM workers WHERE id = ?1",
            params![id],
            |row| {
                let topics_json: String = row.get(1)?;
                Ok(Worker {
                    id: row.get(0)?,
                    topics: serde_json::from_str(&topics_json).unwrap_or_default(),
                    online: row.get::<_, i64>(2)? != 0,
                })
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound)
    }

    /// Lists items, most recently updated first, optionally filtered by
    /// topic (exact match — prefix matching is a worker-side concern, see
    /// [`crate::domain::topic_matches`]) and/or state.
    pub fn list_items(&self, topic: Option<&str>, state: Option<State>) -> Result<Vec<Item>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut sql = String::from(
            "SELECT id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at FROM items WHERE 1=1",
        );
        if topic.is_some() {
            sql.push_str(" AND topic = ?1");
        }
        if state.is_some() {
            sql.push_str(if topic.is_some() {
                " AND state = ?2"
            } else {
                " AND state = ?1"
            });
        }
        sql.push_str(" ORDER BY updated_at DESC");

        let mut stmt = conn.prepare(&sql)?;
        let rows = match (topic, state) {
            (Some(t), Some(s)) => {
                stmt.query_map(params![t, s.as_str()], item_from_row_without_tags)?
            }
            (Some(t), None) => stmt.query_map(params![t], item_from_row_without_tags)?,
            (None, Some(s)) => stmt.query_map(params![s.as_str()], item_from_row_without_tags)?,
            (None, None) => stmt.query_map(params![], item_from_row_without_tags)?,
        };
        let items: Vec<Item> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        attach_tags(&conn, items).map_err(Into::into)
    }

    /// Atomically transitions `open -> claimed` for `worker_id`. Fails with
    /// [`StoreError::Conflict`] if the item was not `open` (already claimed,
    /// or in a later state) — the case this exists to make exclusive.
    pub fn claim_item(&self, id: &str, worker_id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'claimed', assignee = ?1, updated_at = ?2
             WHERE id = ?3 AND state = 'open'",
            params![worker_id, now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "claim")?);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically transitions `claimed -> resolved`. Only the current
    /// assignee may submit.
    pub fn submit_item(&self, id: &str, worker_id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'resolved', updated_at = ?1
             WHERE id = ?2 AND state = 'claimed' AND assignee = ?3",
            params![now, id, worker_id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "submit")?);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically transitions `resolved -> closed` with `resolution = done`
    /// — the requester's approval.
    pub fn approve_item(&self, id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'closed', resolution = 'done', updated_at = ?1
             WHERE id = ?2 AND state = 'resolved'",
            params![now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "approve")?);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Admin operation: closes an item created by mistake, with
    /// `resolution = invalid` ([architecture.md](../../../docs/architecture.md)'s
    /// admin-operation mapping). Unlike `approve_item`, this isn't gated on
    /// reaching `resolved` first — a mistaken item is caught at any
    /// pre-closed stage — and it's assignee-agnostic, matching `approve_item`.
    pub fn remove_item(&self, id: &str) -> Result<Item> {
        self.close_with_resolution(id, Resolution::Invalid, "remove")
    }

    /// Admin operation: closes an item as a duplicate of another, with
    /// `resolution = duplicate`. Same any-pre-closed-state, assignee-agnostic
    /// rules as `remove_item` — see its doc comment.
    pub fn merge_item(&self, id: &str) -> Result<Item> {
        self.close_with_resolution(id, Resolution::Duplicate, "merge")
    }

    /// Admin operation: closes an item that's become irrelevant, with
    /// `resolution = wontfix`. Same any-pre-closed-state, assignee-agnostic
    /// rules as `remove_item` — see its doc comment.
    pub fn force_close_item(&self, id: &str) -> Result<Item> {
        self.close_with_resolution(id, Resolution::Wontfix, "force-close")
    }

    fn close_with_resolution(&self, id: &str, resolution: Resolution, op: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'closed', resolution = ?1, updated_at = ?2
             WHERE id = ?3 AND state != 'closed'",
            params![resolution.as_str(), now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, op)?);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Adds `tags` to an item. Idempotent — already-present tags are
    /// silently skipped (`INSERT OR IGNORE`). Returns the item's full tag
    /// set after the add. Bumps `updated_at` only if a tag was actually
    /// new — a fully-idempotent call is not activity.
    pub fn add_tags(&self, item_id: &str, tags: &[String]) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        require_item(&conn, item_id)?;
        let mut changed = false;
        for tag in tags {
            let affected = conn.execute(
                "INSERT OR IGNORE INTO item_tags (item_id, tag) VALUES (?1, ?2)",
                params![item_id, tag],
            )?;
            changed |= affected > 0;
        }
        if changed {
            conn.execute(
                "UPDATE items SET updated_at = ?1 WHERE id = ?2",
                params![now_millis(), item_id],
            )?;
        }
        tags_for_item(&conn, item_id).map_err(Into::into)
    }

    /// Removes `tags` from an item. Idempotent — removing an absent tag is
    /// not an error. Returns the item's full tag set after the removal.
    /// Bumps `updated_at` only if a tag was actually removed.
    pub fn remove_tags(&self, item_id: &str, tags: &[String]) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut changed = false;
        for tag in tags {
            let affected = conn.execute(
                "DELETE FROM item_tags WHERE item_id = ?1 AND tag = ?2",
                params![item_id, tag],
            )?;
            changed |= affected > 0;
        }
        if changed {
            conn.execute(
                "UPDATE items SET updated_at = ?1 WHERE id = ?2",
                params![now_millis(), item_id],
            )?;
        }
        tags_for_item(&conn, item_id).map_err(Into::into)
    }

    /// Existing tag vocabulary, most-used first, optionally scoped to items
    /// under an exact-match topic. Meant to be called before drafting a new
    /// item so a caller reuses an existing tag string instead of inventing
    /// a synonym.
    pub fn list_tags(&self, topic: Option<&str>) -> Result<Vec<TagCount>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let sql = if topic.is_some() {
            "SELECT it.tag, COUNT(*) FROM item_tags it
             JOIN items i ON i.id = it.item_id
             WHERE i.topic = ?1
             GROUP BY it.tag ORDER BY COUNT(*) DESC, it.tag ASC"
        } else {
            "SELECT tag, COUNT(*) FROM item_tags
             GROUP BY tag ORDER BY COUNT(*) DESC, tag ASC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = match topic {
            Some(t) => stmt.query_map(params![t], tag_count_from_row)?,
            None => stmt.query_map([], tag_count_from_row)?,
        };
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// A comment is always new activity (no idempotency to consider, unlike
    /// `add_tags`/`remove_tags`), so this unconditionally bumps the parent
    /// item's `updated_at` — a thread's most recent comment counts as its
    /// most recent activity for recency sorting.
    pub fn add_comment(&self, item_id: &str, author: &str, body: &str) -> Result<Comment> {
        let id = Uuid::new_v4().to_string();
        let now = now_millis();
        let conn = self.conn.lock().expect("store mutex poisoned");
        require_item(&conn, item_id)?;
        conn.execute(
            "INSERT INTO item_comments (id, item_id, author, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, item_id, author, body, now],
        )?;
        conn.execute(
            "UPDATE items SET updated_at = ?1 WHERE id = ?2",
            params![now, item_id],
        )?;
        Ok(Comment {
            id,
            item_id: item_id.to_string(),
            author: author.to_string(),
            body: body.to_string(),
            created_at: now,
        })
    }

    pub fn list_comments(&self, item_id: &str) -> Result<Vec<Comment>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, item_id, author, body, created_at FROM item_comments
             WHERE item_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![item_id], |row| {
            Ok(Comment {
                id: row.get(0)?,
                item_id: row.get(1)?,
                author: row.get(2)?,
                body: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// `list_items`'s superset: adds tag filtering (`tags`/`tag_match`) and
    /// full-text search (`query`, matched against title+body via `items_fts`
    /// or a comment's body via `comments_fts` — an item whose thread
    /// mentions the term is as findable as one whose title does).
    /// `list_items` itself is untouched — this is a separate method so its
    /// existing callers/tests can't regress.
    pub fn search_items(
        &self,
        topic: Option<&str>,
        state: Option<State>,
        tags: &[String],
        tag_match: TagMatch,
        query: Option<&str>,
    ) -> Result<Vec<Item>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut sql = String::from(
            "SELECT i.id, i.topic, i.title, i.body, i.state, i.resolution, i.requester, i.assignee, i.created_at, i.updated_at
             FROM items i WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(t) = topic {
            sql.push_str(" AND i.topic = ?");
            args.push(Box::new(t.to_string()));
        }
        if let Some(s) = state {
            sql.push_str(" AND i.state = ?");
            args.push(Box::new(s.as_str().to_string()));
        }
        // FTS5 parses its own query syntax out of the raw string, so ordinary
        // search terms (`severity:medium`, `awaiting-release`, `@scope/name`)
        // are syntax errors rather than searches. Wrapping the input in an
        // FTS5 phrase literal makes the whole thing match as plain text. A
        // blank query constrains nothing, so it is treated as absent.
        //
        // Matches against either items_fts (title/body) or comments_fts (a
        // comment's body, joined back to its item via item_id) — a thread's
        // conversation is as searchable as its opening title/body, not just
        // the part that happened to be written first.
        if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty()) {
            sql.push_str(
                " AND (i.rowid IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)
                   OR i.id IN (SELECT item_id FROM item_comments WHERE rowid IN
                       (SELECT rowid FROM comments_fts WHERE comments_fts MATCH ?)))",
            );
            let phrase = format!("\"{}\"", q.replace('"', "\"\""));
            args.push(Box::new(phrase.clone()));
            args.push(Box::new(phrase));
        }
        if !tags.is_empty() {
            let placeholders = std::iter::repeat_n("?", tags.len())
                .collect::<Vec<_>>()
                .join(", ");
            match tag_match {
                TagMatch::Any => {
                    sql.push_str(&format!(
                        " AND i.id IN (SELECT item_id FROM item_tags WHERE tag IN ({placeholders}))"
                    ));
                }
                TagMatch::All => {
                    // Counted against the *distinct* input tags to match
                    // COUNT(DISTINCT tag): a caller asking for ["a", "a"]
                    // means "all of: a", which no item could satisfy if the
                    // required count were the raw slice length.
                    let required: std::collections::HashSet<&String> = tags.iter().collect();
                    sql.push_str(&format!(
                        " AND i.id IN (SELECT item_id FROM item_tags WHERE tag IN ({placeholders})
                           GROUP BY item_id HAVING COUNT(DISTINCT tag) = {})",
                        required.len()
                    ));
                }
            }
            for tag in tags {
                args.push(Box::new(tag.clone()));
            }
        }
        sql.push_str(" ORDER BY i.updated_at DESC");

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(
            rusqlite::params_from_iter(param_refs),
            item_from_row_without_tags,
        )?;
        let items: Vec<Item> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        attach_tags(&conn, items).map_err(Into::into)
    }
}

/// Upgrades a database created before [ADR-0010](../../../docs/decisions/ADR-0010-item-from-to-turn.md):
/// `items.owner` becomes `items.assignee`, and a new nullable `items.requester`
/// column is added. `CREATE TABLE IF NOT EXISTS` above already gives a fresh
/// database both new columns directly, so this only fires for a database that
/// still has the pre-ADR-0010 `owner` column — checked via `pragma_table_info`
/// rather than tracking a schema-version number, since that's the one fact
/// this migration actually depends on. Idempotent: a database already
/// migrated (or created fresh) has no `owner` column, so this is a no-op.
fn migrate_owner_to_requester_assignee(conn: &Connection) -> rusqlite::Result<()> {
    let has_owner_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('items') WHERE name = 'owner'")?
        .exists([])?;
    if has_owner_column {
        conn.execute_batch(
            "ALTER TABLE items RENAME COLUMN owner TO assignee;
             ALTER TABLE items ADD COLUMN requester TEXT;",
        )?;
    }
    Ok(())
}

fn existing_state_conflict(conn: &Connection, id: &str, op: &str) -> Result<StoreError> {
    match row_to_item(conn, id)? {
        Some(item) => Ok(StoreError::Conflict(format!(
            "cannot {op}: item is {}",
            item.state.as_str()
        ))),
        None => Ok(StoreError::NotFound),
    }
}

/// Guards writes to an item's child tables. Without it a typo'd id reaches
/// the insert, which is indistinguishable from a server fault to the caller —
/// unlike `remove_tags`/`list_comments`, which already answer "no such item"
/// with an empty result rather than an error.
fn require_item(conn: &Connection, item_id: &str) -> Result<()> {
    conn.query_row(
        "SELECT 1 FROM items WHERE id = ?1",
        params![item_id],
        |_| Ok(()),
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

fn row_to_item(conn: &Connection, id: &str) -> Result<Option<Item>> {
    let item = conn
        .query_row(
            "SELECT id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at
             FROM items WHERE id = ?1",
            params![id],
            item_from_row_without_tags,
        )
        .optional()?;
    match item {
        Some(item) => Ok(Some(attach_tags(conn, vec![item])?.remove(0))),
        None => Ok(None),
    }
}

/// Reads every non-tag column. Safe to use inside `query_map` closures
/// because it never re-borrows `conn`. Expects the column order every SQL
/// string in this module uses: `id, topic, title, body, state, resolution,
/// requester, assignee, created_at, updated_at`.
fn item_from_row_without_tags(row: &rusqlite::Row) -> rusqlite::Result<Item> {
    let state_str: String = row.get(4)?;
    let resolution_str: Option<String> = row.get(5)?;
    let state = State::parse(&state_str).expect("state column always holds a valid State");
    Ok(Item {
        id: row.get(0)?,
        topic: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        state,
        resolution: resolution_str.and_then(|s| Resolution::parse(&s)),
        requester: row.get(6)?,
        assignee: row.get(7)?,
        turn: Item::turn_for(state),
        tags: Vec::new(),
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// Runs after the `Statement` borrow of `conn` has ended, filling in each
/// item's `tags` with a second query per item (N+1 — fine at this project's
/// scale, see principles.md "Simplicity > reliability > scalability").
fn attach_tags(conn: &Connection, mut items: Vec<Item>) -> rusqlite::Result<Vec<Item>> {
    for item in &mut items {
        item.tags = tags_for_item(conn, &item.id)?;
    }
    Ok(items)
}

fn tags_for_item(conn: &Connection, item_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT tag FROM item_tags WHERE item_id = ?1 ORDER BY tag")?;
    let rows = stmt.query_map(params![item_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

fn tag_count_from_row(row: &rusqlite::Row) -> rusqlite::Result<TagCount> {
    Ok(TagCount {
        tag: row.get(0)?,
        count: row.get(1)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn open_test_store() -> Store {
        Store::open(":memory:").expect("in-memory store opens")
    }

    #[test]
    fn full_lifecycle_open_to_closed() {
        let store = open_test_store();
        store
            .register_worker("w1", &["iyulab".to_string()])
            .unwrap();
        let item = store
            .create_item("iyulab/docket", "fix bug", None, &[], None)
            .unwrap();
        assert_eq!(item.state, State::Open);

        let claimed = store.claim_item(&item.id, "w1").unwrap();
        assert_eq!(claimed.state, State::Claimed);
        assert_eq!(claimed.assignee.as_deref(), Some("w1"));

        let resolved = store.submit_item(&item.id, "w1").unwrap();
        assert_eq!(resolved.state, State::Resolved);

        let closed = store.approve_item(&item.id).unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Done));
    }

    #[test]
    fn remove_item_closes_an_open_item_as_invalid() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(item.state, State::Open);

        let closed = store.remove_item(&item.id).unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Invalid));
    }

    #[test]
    fn merge_item_closes_a_claimed_item_as_duplicate() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();

        let closed = store.merge_item(&item.id).unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Duplicate));
        // owner-agnostic — the claim it interrupted is preserved as history
        assert_eq!(closed.assignee.as_deref(), Some("w1"));
    }

    #[test]
    fn force_close_item_closes_a_resolved_item_as_wontfix() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1").unwrap();

        let closed = store.force_close_item(&item.id).unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Wontfix));
    }

    #[test]
    fn admin_close_ops_reject_an_already_closed_item() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.remove_item(&item.id).unwrap();

        assert!(matches!(
            store.merge_item(&item.id).unwrap_err(),
            StoreError::Conflict(_)
        ));
        assert!(matches!(
            store.force_close_item(&item.id).unwrap_err(),
            StoreError::Conflict(_)
        ));
        assert!(matches!(
            store.remove_item(&item.id).unwrap_err(),
            StoreError::Conflict(_)
        ));
    }

    #[test]
    fn admin_close_ops_report_not_found_for_a_missing_item() {
        let store = open_test_store();
        assert!(matches!(
            store.remove_item("nope").unwrap_err(),
            StoreError::NotFound
        ));
        assert!(matches!(
            store.merge_item("nope").unwrap_err(),
            StoreError::NotFound
        ));
        assert!(matches!(
            store.force_close_item("nope").unwrap_err(),
            StoreError::NotFound
        ));
    }

    #[test]
    fn claim_rejects_already_claimed() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        let err = store.claim_item(&item.id, "w2").unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[test]
    fn submit_rejects_non_owner() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        let err = store.submit_item(&item.id, "w2").unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    /// The M1 completion criterion: if two workers race to claim the same
    /// item, exactly one succeeds.
    #[test]
    fn concurrent_claims_exactly_one_winner() {
        let store = Arc::new(open_test_store());
        let item = store
            .create_item("iyulab/docket", "race", None, &[], None)
            .unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = Arc::clone(&store);
                let item_id = item.id.clone();
                std::thread::spawn(move || store.claim_item(&item_id, &format!("w{i}")).is_ok())
            })
            .collect();

        let wins = handles
            .into_iter()
            .map(|h| h.join().expect("claim thread panicked"))
            .filter(|ok| *ok)
            .count();

        assert_eq!(wins, 1);
        assert_eq!(store.get_item(&item.id).unwrap().state, State::Claimed);
    }

    fn temp_db_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "docket-core-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fts5_index_stays_in_sync_with_items_via_triggers() {
        let dir = temp_db_dir("fts5-test");
        let db_path = dir.join("fts5.db");

        // Store::open runs the schema/trigger migration, then is dropped —
        // this task only needs the migration to have run once.
        drop(Store::open(db_path.to_str().unwrap()).unwrap());

        // A second raw connection to the same file exercises the triggers
        // exactly as any writer would, without needing any `Store` method
        // this task doesn't own (`create_item`'s tags param and
        // `search_items` are added in Task 2).
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO items (id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at)
             VALUES ('t1', 'iyulab/node-packages', 'form Enter bypasses preventDefault',
                     'trusted keydown events navigate anyway', 'open', NULL, NULL, NULL, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items (id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at)
             VALUES ('t2', 'iyulab/node-packages', 'unrelated item', NULL, 'open', NULL, NULL, NULL, 0, 0)",
            [],
        )
        .unwrap();

        let matched: Vec<i64> = conn
            .prepare("SELECT rowid FROM items_fts WHERE items_fts MATCH 'preventDefault'")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(matched.len(), 1);

        // The `AFTER DELETE` trigger must remove the corresponding
        // items_fts row too, not leave a stale entry behind.
        conn.execute("DELETE FROM items WHERE id = 't1'", [])
            .unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'preventDefault'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);

        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A row written before `items_fts` existed is not covered by the AFTER
    /// INSERT trigger, so `open` has to index it retroactively. Both halves
    /// matter: an un-indexed row is invisible to search *and* unwritable,
    /// because the AFTER UPDATE trigger's 'delete' command against a missing
    /// index entry fails and rolls the whole write back.
    #[test]
    fn open_indexes_rows_written_before_the_fts_migration() {
        let dir = temp_db_dir("legacy-migration-test");
        let db_path = dir.join("legacy.db");

        // The schema exactly as it stood before item_tags/item_comments/
        // items_fts were introduced.
        let legacy = Connection::open(&db_path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE workers (
                    id TEXT PRIMARY KEY,
                    topics TEXT NOT NULL,
                    online INTEGER NOT NULL,
                    registered_at INTEGER NOT NULL
                );
                CREATE TABLE items (
                    id TEXT PRIMARY KEY,
                    topic TEXT NOT NULL,
                    title TEXT NOT NULL,
                    body TEXT,
                    state TEXT NOT NULL,
                    resolution TEXT,
                    owner TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO items (id, topic, title, body, state, resolution, owner, created_at, updated_at)
                 VALUES ('legacy-1', 'iyulab/docket', 'hydration mismatch on first paint',
                         NULL, 'open', NULL, NULL, 0, 0)",
                [],
            )
            .unwrap();
        drop(legacy);

        let store = Store::open(db_path.to_str().unwrap()).unwrap();

        let found = store
            .search_items(None, None, &[], TagMatch::Any, Some("hydration"))
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "legacy-1");

        let claimed = store.claim_item("legacy-1", "w1").unwrap();
        assert_eq!(claimed.state, State::Claimed);
        // The pre-ADR-0010 `owner` column migrated to `assignee` and got
        // written by claim_item exactly as a fresh-schema row would.
        assert_eq!(claimed.assignee.as_deref(), Some("w1"));
        assert_eq!(claimed.requester, None);

        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Dedicated migration test (distinct from the FTS-migration test above,
    /// which only needs a fresh `owner`-free schema): a database that still
    /// has the pre-ADR-0010 `owner` column gets it renamed to `assignee` and
    /// gains a new `requester` column, without losing existing data.
    #[test]
    fn open_migrates_owner_column_to_requester_and_assignee() {
        let dir = temp_db_dir("owner-migration-test");
        let db_path = dir.join("pre-adr-0010.db");

        let legacy = Connection::open(&db_path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE items (
                    id TEXT PRIMARY KEY,
                    topic TEXT NOT NULL,
                    title TEXT NOT NULL,
                    body TEXT,
                    state TEXT NOT NULL,
                    resolution TEXT,
                    owner TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO items (id, topic, title, body, state, resolution, owner, created_at, updated_at)
                 VALUES ('pre-1', 'iyulab/docket', 'pre-migration item', NULL, 'claimed', NULL, 'w1', 0, 0)",
                [],
            )
            .unwrap();
        drop(legacy);

        let store = Store::open(db_path.to_str().unwrap()).unwrap();
        let item = store.get_item("pre-1").unwrap();
        assert_eq!(item.assignee.as_deref(), Some("w1"));
        assert_eq!(item.requester, None);

        // Idempotent: opening an already-migrated database again must not
        // error (there is no `owner` column left to rename a second time).
        drop(store);
        Store::open(db_path.to_str().unwrap()).unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_item_rejects_blank_topic_or_title() {
        let store = open_test_store();
        assert!(matches!(
            store.create_item("", "t", None, &[], None),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            store.create_item("   ", "t", None, &[], None),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            store.create_item("iyulab/docket", "", None, &[], None),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            store.create_item("iyulab/docket", "  ", None, &[], None),
            Err(StoreError::Validation(_))
        ));
    }

    #[test]
    fn create_item_trims_topic_title_and_requester() {
        let store = open_test_store();
        let item = store
            .create_item(
                "  iyulab/docket  ",
                "  t  ",
                None,
                &[],
                Some("  reporter-1  "),
            )
            .unwrap();
        assert_eq!(item.topic, "iyulab/docket");
        assert_eq!(item.title, "t");
        assert_eq!(item.requester.as_deref(), Some("reporter-1"));
    }

    #[test]
    fn set_item_from_updates_requester_and_bumps_updated_at() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(item.requester, None);
        let created_updated_at = item.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        let updated = store
            .set_item_from(&item.id, "  backfilled-reporter  ")
            .unwrap();
        assert_eq!(updated.requester.as_deref(), Some("backfilled-reporter"));
        assert!(updated.updated_at > created_updated_at);
    }

    #[test]
    fn set_item_from_works_on_a_closed_item() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.remove_item(&item.id).unwrap();

        let updated = store.set_item_from(&item.id, "reporter-1").unwrap();
        assert_eq!(updated.state, State::Closed);
        assert_eq!(updated.requester.as_deref(), Some("reporter-1"));
    }

    #[test]
    fn set_item_from_rejects_blank_and_missing_item() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert!(matches!(
            store.set_item_from(&item.id, "   "),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            store.set_item_from("nonexistent-id", "reporter-1"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn create_item_with_tags_round_trips() {
        let store = open_test_store();
        let item = store
            .create_item(
                "iyulab/node-packages",
                "t",
                None,
                &[
                    "severity:medium".to_string(),
                    "evidence:reproduced".to_string(),
                ],
                None,
            )
            .unwrap();
        let mut tags = item.tags.clone();
        tags.sort();
        assert_eq!(tags, vec!["evidence:reproduced", "severity:medium"]);

        let fetched = store.get_item(&item.id).unwrap();
        let mut fetched_tags = fetched.tags;
        fetched_tags.sort();
        assert_eq!(fetched_tags, vec!["evidence:reproduced", "severity:medium"]);
    }

    #[test]
    fn add_tags_is_idempotent() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store
            .add_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        let tags = store
            .add_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        assert_eq!(tags, vec!["awaiting-release"]);
    }

    #[test]
    fn add_tags_bumps_updated_at_only_when_a_tag_is_actually_new() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        let created_updated_at = item.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .add_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        let after_new_tag = store.get_item(&item.id).unwrap().updated_at;
        assert!(after_new_tag > created_updated_at);

        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .add_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        let after_duplicate_tag = store.get_item(&item.id).unwrap().updated_at;
        assert_eq!(after_duplicate_tag, after_new_tag);
    }

    #[test]
    fn remove_tags_is_idempotent() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store
            .add_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        let tags = store
            .remove_tags(
                &item.id,
                &["awaiting-release".to_string(), "never-added".to_string()],
            )
            .unwrap();
        assert!(tags.is_empty());
        // Removing again is not an error.
        let tags = store
            .remove_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        assert!(tags.is_empty());
    }

    #[test]
    fn remove_tags_bumps_updated_at_only_when_a_tag_is_actually_removed() {
        let store = open_test_store();
        let item = store
            .create_item(
                "iyulab/docket",
                "t",
                None,
                &["awaiting-release".to_string()],
                None,
            )
            .unwrap();
        let created_updated_at = item.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .remove_tags(&item.id, &["never-added".to_string()])
            .unwrap();
        let after_noop_remove = store.get_item(&item.id).unwrap().updated_at;
        assert_eq!(after_noop_remove, created_updated_at);

        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .remove_tags(&item.id, &["awaiting-release".to_string()])
            .unwrap();
        let after_real_remove = store.get_item(&item.id).unwrap().updated_at;
        assert!(after_real_remove > created_updated_at);
    }

    #[test]
    fn list_tags_counts_and_orders_by_frequency() {
        let store = open_test_store();
        let a = store
            .create_item(
                "iyulab/node-packages",
                "a",
                None,
                &["blocked".to_string()],
                None,
            )
            .unwrap();
        let b = store
            .create_item(
                "iyulab/node-packages",
                "b",
                None,
                &["blocked".to_string()],
                None,
            )
            .unwrap();
        store
            .create_item("iyulab/router", "c", None, &["deferred".to_string()], None)
            .unwrap();
        let _ = (a.id, b.id);

        let all = store.list_tags(None).unwrap();
        assert_eq!(all[0].tag, "blocked");
        assert_eq!(all[0].count, 2);

        let scoped = store.list_tags(Some("iyulab/router")).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].tag, "deferred");
    }

    #[test]
    fn search_items_filters_by_tag_match_any_and_all() {
        let store = open_test_store();
        let both = store
            .create_item(
                "iyulab/docket",
                "both",
                None,
                &["a".to_string(), "b".to_string()],
                None,
            )
            .unwrap();
        let only_a = store
            .create_item("iyulab/docket", "only-a", None, &["a".to_string()], None)
            .unwrap();

        let any_match = store
            .search_items(
                None,
                None,
                &["a".to_string(), "b".to_string()],
                TagMatch::Any,
                None,
            )
            .unwrap();
        let mut any_ids: Vec<_> = any_match.iter().map(|i| i.id.clone()).collect();
        any_ids.sort();
        let mut expected_any = vec![both.id.clone(), only_a.id.clone()];
        expected_any.sort();
        assert_eq!(any_ids, expected_any);

        let all_match = store
            .search_items(
                None,
                None,
                &["a".to_string(), "b".to_string()],
                TagMatch::All,
                None,
            )
            .unwrap();
        assert_eq!(all_match.len(), 1);
        assert_eq!(all_match[0].id, both.id);
    }

    #[test]
    fn search_items_combines_topic_state_and_query() {
        let store = open_test_store();
        store
            .create_item("iyulab/node-packages", "form bug", None, &[], None)
            .unwrap();
        store
            .create_item("iyulab/router", "form bug elsewhere", None, &[], None)
            .unwrap();

        let results = store
            .search_items(
                Some("iyulab/node-packages"),
                Some(State::Open),
                &[],
                TagMatch::Any,
                Some("form"),
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].topic, "iyulab/node-packages");
    }

    #[test]
    fn add_comment_then_list_comments_in_order() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        let first = store
            .add_comment(&item.id, "requester", "please look at this")
            .unwrap();
        let second = store
            .add_comment(&item.id, "maintainer", "root cause found")
            .unwrap();

        let comments = store.list_comments(&item.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, first.id);
        assert_eq!(comments[0].author, "requester");
        assert_eq!(comments[1].id, second.id);
        assert_eq!(comments[1].author, "maintainer");
    }

    #[test]
    fn add_comment_bumps_item_updated_at() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        let created_updated_at = item.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .add_comment(&item.id, "requester", "please look at this")
            .unwrap();

        let after_comment = store.get_item(&item.id).unwrap().updated_at;
        assert!(after_comment > created_updated_at);
    }

    #[test]
    fn list_comments_on_item_with_none_is_empty() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert!(store.list_comments(&item.id).unwrap().is_empty());
    }

    /// A term that appears only in a comment, never in the item's own
    /// title/body, must still surface the item — a thread's conversation is
    /// as searchable as its opening post.
    #[test]
    fn search_items_matches_comment_body() {
        let store = open_test_store();
        let matching = store
            .create_item("iyulab/docket", "unrelated title", None, &[], None)
            .unwrap();
        let other = store
            .create_item("iyulab/docket", "also unrelated", None, &[], None)
            .unwrap();
        store
            .add_comment(
                &matching.id,
                "maintainer",
                "root cause is a race in claim_item",
            )
            .unwrap();
        store
            .add_comment(&other.id, "maintainer", "unrelated follow-up")
            .unwrap();

        let results = store
            .search_items(None, None, &[], TagMatch::Any, Some("race in claim_item"))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, matching.id);
    }

    /// A row written before `comments_fts` existed is not covered by its
    /// AFTER INSERT trigger — same backfill requirement as
    /// `open_indexes_rows_written_before_the_fts_migration`, applied to
    /// comments instead of items.
    #[test]
    fn open_indexes_comments_written_before_the_comments_fts_migration() {
        let dir = temp_db_dir("legacy-comments-migration-test");
        let db_path = dir.join("legacy.db");

        // The schema exactly as it stood before comments_fts was introduced:
        // items_fts already exists, item_comments already exists, but
        // comments_fts and its trigger do not.
        let legacy = Connection::open(&db_path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE items (
                    id TEXT PRIMARY KEY,
                    topic TEXT NOT NULL,
                    title TEXT NOT NULL,
                    body TEXT,
                    state TEXT NOT NULL,
                    resolution TEXT,
                    owner TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE item_comments (
                    id         TEXT PRIMARY KEY,
                    item_id    TEXT NOT NULL REFERENCES items(id),
                    author     TEXT NOT NULL,
                    body       TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO items (id, topic, title, body, state, resolution, owner, created_at, updated_at)
                 VALUES ('legacy-1', 'iyulab/docket', 'unrelated title', NULL, 'open', NULL, NULL, 0, 0)",
                [],
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO item_comments (id, item_id, author, body, created_at)
                 VALUES ('c1', 'legacy-1', 'maintainer', 'root cause is a race in claim_item', 0)",
                [],
            )
            .unwrap();
        drop(legacy);

        let store = Store::open(db_path.to_str().unwrap()).unwrap();

        let found = store
            .search_items(None, None, &[], TagMatch::Any, Some("race in claim_item"))
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "legacy-1");

        // The AFTER INSERT trigger must also work going forward on a
        // backfilled table, not just the retroactive rebuild.
        store
            .add_comment("legacy-1", "requester", "thanks for the fix")
            .unwrap();
        let found2 = store
            .search_items(None, None, &[], TagMatch::Any, Some("thanks for the fix"))
            .unwrap();
        assert_eq!(found2.len(), 1);

        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
