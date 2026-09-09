use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{
    Alias, AliasMap, Comment, Item, RelatedItemRef, RelatedRelation, Resolution, SortOrder, State,
    TagCount, TagMatch, TopicCount, Worker,
};

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
    /// Bumped by `invalidate_alias_cache` on every alias-table mutation.
    /// `alias_cache` records the generation it was built from, and
    /// `alias_map` re-checks this counter after reading rows, before storing
    /// them: releasing `conn` and then separately taking the cache's write
    /// lock leaves a window where a concurrent mutation lands and invalidates
    /// in between, and without this check the read that started before the
    /// mutation would overwrite that invalidation with the data it fetched
    /// beforehand. A monotonically increasing counter (rather than clearing
    /// the cache to `None`) makes that check possible: two loaders can tell
    /// which of them read a fresher snapshot.
    alias_generation: std::sync::atomic::AtomicU64,
    /// `identity_aliases` is read on nearly every query and written rarely, so
    /// it is cached whole rather than re-queried per comparison. `None` means
    /// "not loaded yet"; a `Some` is only trusted while its generation still
    /// matches `alias_generation` (see that field's doc comment).
    alias_cache: std::sync::RwLock<Option<(u64, AliasMap)>>,
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
                updated_at INTEGER NOT NULL,
                archived_at INTEGER,
                seq INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_items_topic ON items(topic);
            CREATE INDEX IF NOT EXISTS idx_items_state ON items(state);
            -- Backs `seq`'s allocation (see migrate_add_item_seq / create_item):
            -- a single row advanced under the store's connection-wide mutex, so
            -- `seq` is race-free without any extra locking primitive. Not an
            -- AUTOINCREMENT rowid alias — items.id is already the declared
            -- PRIMARY KEY, and rowid reuse after delete_item would risk handing
            -- a deleted item's number to an unrelated new item. See ADR-0016.
            CREATE TABLE IF NOT EXISTS seq_counter (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                next INTEGER NOT NULL
            );
            -- One identity, several spellings (ADR-0022). Storage keeps what
            -- was written everywhere else (ADR-0021); this table is what lets
            -- comparison fold two spellings without rewriting a single row,
            -- which is why declaring an alias applies retroactively.
            -- COLLATE NOCASE is declared on the column (not left to each
            -- query) because every query against this table already compares
            -- with COLLATE NOCASE, and SQLite only uses an index whose
            -- collation matches the comparison — without this, both the PK
            -- and the canonical index below are unusable, and the PK does
            -- not enforce case-insensitive uniqueness on alias. This
            -- diverges from the workers table's default-collation pattern
            -- deliberately: a later resolution step makes alias resolution
            -- decide who may approve an item, so one alias mapping to
            -- exactly one canonical must be enforced by the schema, not
            -- only by the connection mutex.
            CREATE TABLE IF NOT EXISTS identity_aliases (
                alias      TEXT PRIMARY KEY COLLATE NOCASE,
                canonical  TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_identity_aliases_canonical
                ON identity_aliases(canonical COLLATE NOCASE);
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
            END;
            CREATE TRIGGER IF NOT EXISTS comments_fts_ad AFTER DELETE ON item_comments BEGIN
                INSERT INTO comments_fts(comments_fts, rowid, body) VALUES('delete', old.rowid, old.body);
            END;",
        )?;
        migrate_owner_to_requester_assignee(&conn)?;
        migrate_add_archived_at(&conn)?;
        // Must run after migrate_add_archived_at: on a database that
        // predates archived_at, the CREATE TABLE IF NOT EXISTS above is a
        // no-op (the table already exists), so the column doesn't exist
        // until the migration adds it. Indexing it any earlier would fail
        // with "no such column: archived_at" on exactly the legacy
        // databases this migration exists to upgrade.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_items_archived_at ON items(archived_at)",
            [],
        )?;
        // Resyncs the external-content index against `items` on every open:
        // rows written before this virtual table existed are never covered by
        // the AFTER INSERT trigger, and an un-indexed row makes the AFTER
        // UPDATE trigger's 'delete' command fail the whole write. A row-diffing
        // backfill can't find those rows — `SELECT rowid FROM items_fts` reads
        // through to `items`, so it always reports every row as present.
        // 'rebuild' is idempotent and cheap at this project's scale.
        conn.execute("INSERT INTO items_fts(items_fts) VALUES('rebuild')", [])?;
        // Same rationale as items_fts above, applied to comments_fts.
        // item_comments is still never *edited* (no update API, ADR-0009),
        // so no AFTER UPDATE trigger is needed — an INSERT-only trigger
        // can't fall out of sync with rows that never change once written.
        // Rows can be deleted now, though: delete_item (ADR-0013) cascades
        // to item_comments, which is exactly why the comments_fts_ad
        // trigger above exists — it keeps the FTS index in sync when that
        // happens.
        conn.execute(
            "INSERT INTO comments_fts(comments_fts) VALUES('rebuild')",
            [],
        )?;
        // Must run after both `rebuild`s above, not before: on a legacy
        // database, `items_fts` is freshly created and still empty at this
        // point in `open()` until 'rebuild' populates it. The backfill UPDATE
        // below fires `items_fts_au` (an UPDATE touches the row regardless of
        // which column changed), whose 'delete' command targets a rowid that
        // must already be indexed — exactly the failure mode the 'rebuild'
        // comment above describes ("an un-indexed row makes the AFTER UPDATE
        // trigger's 'delete' command fail the whole write"), observed here as
        // the migration returning `DatabaseCorrupt` when this ran earlier.
        migrate_add_item_seq(&conn)?;
        // Same reasoning as idx_items_archived_at above: must run after the
        // migration, since a legacy database's `seq` column doesn't exist
        // until migrate_add_item_seq adds it. UNIQUE guards against a bug in
        // that backfill ever assigning the same number twice.
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_items_seq ON items(seq)",
            [],
        )?;
        // Seeds a fresh database's counter to 1. A no-op on a database that
        // just ran migrate_add_item_seq's backfill (already inserted its own
        // row, set to continue from the highest existing seq).
        conn.execute(
            "INSERT OR IGNORE INTO seq_counter (id, next) VALUES (1, 1)",
            [],
        )?;
        Ok(Store {
            conn: Mutex::new(conn),
            alias_generation: std::sync::atomic::AtomicU64::new(0),
            alias_cache: std::sync::RwLock::new(None),
        })
    }

    /// Declares that `alias` names the same identity as `canonical`.
    ///
    /// Idempotent for the same pair (case-folded). Rejects, as `Conflict`:
    /// a chain in either direction, and re-pointing an existing alias at a
    /// different canonical — an alias silently changing meaning would move
    /// every item filed under it to a different identity, including who may
    /// approve them (ADR-0019). Blank or self-referential input is
    /// `Validation`, not `Conflict`: that's malformed input, not a state clash.
    pub fn put_alias(&self, alias: &str, canonical: &str) -> Result<Alias> {
        let alias = alias.trim();
        let canonical = canonical.trim();
        if alias.is_empty() || canonical.is_empty() {
            return Err(StoreError::Validation(
                "alias and canonical must not be blank".to_string(),
            ));
        }
        if crate::domain::identity_eq(alias, canonical) {
            return Err(StoreError::Validation(
                "alias must differ from canonical".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let alias_is_a_canonical: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_aliases WHERE canonical = ?1 COLLATE NOCASE)",
            params![alias],
            |row| row.get(0),
        )?;
        if alias_is_a_canonical {
            return Err(StoreError::Conflict(format!(
                "cannot alias {alias}: it is already the canonical of another alias"
            )));
        }
        let canonical_is_an_alias: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_aliases WHERE alias = ?1 COLLATE NOCASE)",
            params![canonical],
            |row| row.get(0),
        )?;
        if canonical_is_an_alias {
            return Err(StoreError::Conflict(format!(
                "cannot point at {canonical}: it is itself an alias"
            )));
        }
        let existing: Option<(String, String, i64)> = conn
            .query_row(
                "SELECT alias, canonical, created_at FROM identity_aliases
                 WHERE alias = ?1 COLLATE NOCASE",
                params![alias],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((stored_alias, stored_canonical, created_at)) = existing {
            if crate::domain::identity_eq(&stored_canonical, canonical) {
                return Ok(Alias {
                    alias: stored_alias,
                    canonical: stored_canonical,
                    created_at,
                });
            }
            return Err(StoreError::Conflict(format!(
                "{alias} is already an alias of {stored_canonical}"
            )));
        }
        let now = now_millis();
        conn.execute(
            "INSERT INTO identity_aliases (alias, canonical, created_at) VALUES (?1, ?2, ?3)",
            params![alias, canonical, now],
        )?;
        drop(conn);
        self.invalidate_alias_cache();
        Ok(Alias {
            alias: alias.to_string(),
            canonical: canonical.to_string(),
            created_at: now,
        })
    }

    /// Every declared alias, newest first. `canonical` filters to one
    /// identity's variants, folded case like every identifier comparison.
    pub fn list_aliases(&self, canonical: Option<&str>) -> Result<Vec<Alias>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut sql =
            String::from("SELECT alias, canonical, created_at FROM identity_aliases WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(c) = canonical {
            sql.push_str(" AND canonical = ? COLLATE NOCASE");
            args.push(Box::new(c.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC, alias ASC");
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(param_refs), |row| {
            Ok(Alias {
                alias: row.get(0)?,
                canonical: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Withdraws a declaration. Not destructive to any item — the items filed
    /// under that spelling simply stop folding into the canonical, the exact
    /// inverse of `put_alias` applying retroactively.
    pub fn delete_alias(&self, alias: &str) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let affected = conn.execute(
            "DELETE FROM identity_aliases WHERE alias = ?1 COLLATE NOCASE",
            params![alias.trim()],
        )?;
        drop(conn);
        if affected == 0 {
            return Err(StoreError::NotFound);
        }
        self.invalidate_alias_cache();
        Ok(())
    }

    /// The declared alias set, cached. Returns a clone: the table is small
    /// (one row per declared spelling) and cloning keeps callers from holding
    /// the lock across SQL, which would deadlock against `self.conn`.
    ///
    /// The generation is read before the cache is consulted and, on a miss,
    /// re-checked after the rows are read but before they're stored — a
    /// mutation that invalidates the cache while this call is still in
    /// flight bumps the generation, which makes that second check fail and
    /// this call skip caching its now-stale read instead of clobbering the
    /// invalidation with it. The rows it returns are still correct for the
    /// generation it observed; only the caching of them is skipped.
    pub fn alias_map(&self) -> Result<AliasMap> {
        use std::sync::atomic::Ordering;
        let generation = self.alias_generation.load(Ordering::Acquire);
        if let Some((cached_generation, map)) = self
            .alias_cache
            .read()
            .expect("alias cache poisoned")
            .as_ref()
            && *cached_generation == generation
        {
            return Ok(map.clone());
        }
        let map = AliasMap::from_rows(self.list_aliases(None)?);
        let mut cache = self.alias_cache.write().expect("alias cache poisoned");
        if self.alias_generation.load(Ordering::Acquire) == generation {
            *cache = Some((generation, map.clone()));
        }
        Ok(map)
    }

    fn invalidate_alias_cache(&self) {
        self.alias_generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Registers a worker, or updates the registration that already exists.
    ///
    /// Idempotent by design — every session calls this on startup. An id that
    /// differs from a registered one only in case is *that* worker
    /// ([ADR-0021](../../../docs/decisions/ADR-0021-case-insensitive-identity.md)),
    /// so it lands on the existing row and the stored spelling wins; the
    /// returned `id` is that canonical spelling, which is how a caller whose
    /// derived id drifted learns the real one. Refusing the registration
    /// instead was considered and rejected: it would stop exactly the session
    /// whose id drifted from registering at all.
    pub fn register_worker(&self, id: &str, topics: &[String]) -> Result<Worker> {
        let topics_json = serde_json::to_string(topics).expect("Vec<String> always serializes");
        let conn = self.conn.lock().expect("store mutex poisoned");
        let canonical: String = conn
            .query_row(
                "SELECT id FROM workers WHERE id = ?1 COLLATE NOCASE",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| id.to_string());
        conn.execute(
            "INSERT INTO workers (id, topics, online, registered_at) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(id) DO UPDATE SET topics = excluded.topics, online = 1",
            params![canonical, topics_json, now_millis()],
        )?;
        Ok(Worker {
            id: canonical,
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
        // Race-free under the store's connection-wide mutex — see
        // seq_counter's schema comment and ADR-0016.
        let seq: i64 = tx.query_row(
            "UPDATE seq_counter SET next = next + 1 WHERE id = 1 RETURNING next - 1",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO items (id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at, seq)
             VALUES (?1, ?2, ?3, ?4, 'open', NULL, ?5, NULL, ?6, ?6, ?7)",
            params![id, topic, title, body, requester, now, seq],
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
            open: true,
            tags: tags.to_vec(),
            created_at: now,
            updated_at: now,
            archived_at: None,
            seq,
        })
    }

    pub fn get_item(&self, id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Corrects `requester` on an item that already exists — whether or not
    /// it already has one.
    ///
    /// Two cases, and both have always been in scope: *backfilling* an item
    /// filed before a requester identity was available (or left blank by a
    /// migration), and *repairing* an identity that drifted — a typo, a
    /// renamed repo, two consumers spelling the same identity differently.
    /// [ADR-0019](../../../docs/decisions/ADR-0019-approve-reject-requester-match.md)
    /// names this the mitigation for exactly that second case, since a drifted
    /// `requester` otherwise hard-fails a legitimate `approve`/`reject`, and
    /// `approve_reject_conflict` points a caller here when it sees one.
    ///
    /// State-independent (works on a closed item too — this corrects metadata,
    /// it isn't a workflow transition). A change is recorded as a lifecycle
    /// comment naming both values, the same way every other why-bearing
    /// operation records its reason: this is the one edit that can silently
    /// move an item between two parties, so "who changed it, from what"
    /// belongs in the thread rather than only in `updated_at`. Setting the
    /// value it already has is a no-op — same idempotency the tag operations
    /// have, and it keeps a re-run from filling the thread with noise.
    pub fn set_item_requester(&self, id: &str, author: &str, requester: &str) -> Result<Item> {
        let requester = requester.trim();
        if requester.is_empty() {
            return Err(StoreError::Validation(
                "requester must not be blank".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let previous = row_to_item(&conn, id)?
            .ok_or(StoreError::NotFound)?
            .requester;
        if previous.as_deref() == Some(requester) {
            return row_to_item(&conn, id)?.ok_or(StoreError::NotFound);
        }
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET requester = ?1, updated_at = ?2 WHERE id = ?3",
            params![requester, now, id],
        )?;
        if affected == 0 {
            return Err(StoreError::NotFound);
        }
        insert_lifecycle_comment(
            &conn,
            id,
            author,
            &format!(
                "requester: {} -> {requester}",
                previous.as_deref().unwrap_or("(unset)")
            ),
            now,
        )?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Sets `archived_at` if not already set. Idempotent — archiving an
    /// already-archived item is not an error, it just returns the item
    /// unchanged (matches `add_tags`/`remove_tags`'s existing idempotency
    /// convention). State-unrestricted: `archived_at` is orthogonal to
    /// `state` (an old, abandoned `open` item is as plausible a candidate
    /// as a `closed` one). See ADR-0013.
    pub fn archive_item(&self, id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        require_item(&conn, id)?;
        let now = now_millis();
        conn.execute(
            "UPDATE items SET archived_at = ?1, updated_at = ?1 WHERE id = ?2 AND archived_at IS NULL",
            params![now, id],
        )?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Hard delete — the item and its tags/comments are gone, not just
    /// closed. No `author`/`reason`: there is nothing left afterward to
    /// attach either to. State-unrestricted, same precedent as
    /// `remove`/`merge`/`force-close`. A caller wanting a *traceable*
    /// removal should use `remove_item` instead, which keeps a permanent
    /// `resolution = invalid` record. See ADR-0013.
    ///
    /// A dangling free-form tag on some *other* item that happens to
    /// reference this item's id by convention (e.g. a `related:<id>`-shaped
    /// tag) is not cleaned up — the core cannot know a tag's string encodes
    /// a reference (`principles.md` P-1: tags are opaque). Accepted,
    /// documented consequence, not a bug.
    pub fn delete_item(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let resolved = resolve_item_id(&tx, id)?;
        let id = resolved.as_str();
        require_item(&tx, id)?;
        tx.execute("DELETE FROM item_tags WHERE item_id = ?1", params![id])?;
        tx.execute("DELETE FROM item_comments WHERE item_id = ?1", params![id])?;
        tx.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_worker(&self, id: &str) -> Result<Worker> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT id, topics, online FROM workers WHERE id = ?1 COLLATE NOCASE",
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
    /// topic (exact match), state, and archived status. `archived: None`
    /// or `Some(false)` excludes archived items (today's default
    /// behavior, unaffected by this parameter's addition); `Some(true)`
    /// returns *only* archived items — the explicit archive-only browse
    /// ADR-0013 sets out. See [`crate::domain::topic_matches`] for prefix
    /// matching, a worker-side concern this method doesn't do.
    pub fn list_items(
        &self,
        topic: Option<&str>,
        state: Option<State>,
        archived: Option<bool>,
        order: SortOrder,
    ) -> Result<Vec<Item>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut sql = String::from(
            "SELECT id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at, archived_at, seq FROM items WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(t) = topic {
            sql.push_str(" AND topic = ? COLLATE NOCASE");
            args.push(Box::new(t.to_string()));
        }
        if let Some(s) = state {
            sql.push_str(" AND state = ?");
            args.push(Box::new(s.as_str().to_string()));
        }
        if archived.unwrap_or(false) {
            sql.push_str(" AND archived_at IS NOT NULL");
        } else {
            sql.push_str(" AND archived_at IS NULL");
        }
        sql.push_str(" ORDER BY updated_at ");
        sql.push_str(order.sql_keyword());

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(
            rusqlite::params_from_iter(param_refs),
            item_from_row_without_tags,
        )?;
        let items: Vec<Item> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        attach_tags(&conn, items).map_err(Into::into)
    }

    /// Atomically transitions `open -> claimed` for `worker_id`. Fails with
    /// [`StoreError::Conflict`] if the item was not `open` (already claimed,
    /// or in a later state) — the case this exists to make exclusive.
    pub fn claim_item(&self, id: &str, worker_id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
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

    /// Atomically transitions `claimed -> resolved` — handing the turn to the
    /// requester. Only the current assignee may submit.
    ///
    /// `resolved` means "the assignee cannot take this further; the requester
    /// decides what happens next", which covers finished work *and* work that
    /// is blocked on an answer only the requester has. Both are the same fact
    /// about whose turn it is, and this is the only transition that produces
    /// it — see ADR-0010's 2026-09-08 update.
    ///
    /// `reason` is optional and recorded as an atomic lifecycle comment when
    /// present, the same way `reject_item`/`reopen_item` record their required
    /// ones. Optional rather than required because a submission often has
    /// nothing to add beyond the transition itself ("done, as described"),
    /// while a question does — and leaving that to a separate `add_comment`
    /// call would put the intent outside the transition that carries it. Blank
    /// or whitespace-only counts as absent, the same treatment every other
    /// optional string in this crate gets.
    pub fn submit_item(&self, id: &str, worker_id: &str, reason: Option<&str>) -> Result<Item> {
        let reason = reason.map(str::trim).filter(|r| !r.is_empty());
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'resolved', updated_at = ?1
             WHERE id = ?2 AND state = 'claimed' AND assignee = ?3 COLLATE NOCASE",
            params![now, id, worker_id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "submit")?);
        }
        if let Some(reason) = reason {
            insert_lifecycle_comment(&conn, id, worker_id, reason, now)?;
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically transitions `resolved -> claimed` — the requester handing
    /// the turn back to the assignee. Rework is one reason; *answering a
    /// question the assignee submitted* is another, and both are ordinary use
    /// (ADR-0010's 2026-09-08 update). `assignee` is unchanged. `reason` is
    /// required and recorded as an atomic comment — which is what carries the
    /// answer, so the round trip needs no state beyond these two transitions
    /// (same two-`execute`-under-one-lock pattern as `add_comment` — no
    /// separate `conn.transaction()` needed, since no other thread can
    /// interleave while this lock is held). See ADR-0012.
    ///
    /// Only the item's `requester` may reject (mirrors `submit_item`'s
    /// `assignee` match on the other side of the handshake) — unless
    /// `requester` is unset, in which case there is no party to violate. See
    /// [ADR-0019](../../../docs/decisions/ADR-0019-approve-reject-requester-match.md).
    pub fn reject_item(&self, id: &str, author: &str, reason: &str) -> Result<Item> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(StoreError::Validation(
                "reason must not be blank".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'claimed', updated_at = ?1
             WHERE id = ?2 AND state = 'resolved' AND (requester IS NULL OR requester = ?3 COLLATE NOCASE)",
            params![now, id, author],
        )?;
        if affected == 0 {
            return Err(approve_reject_conflict(&conn, id, "reject", author)?);
        }
        insert_lifecycle_comment(&conn, id, author, reason, now)?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically leaves `closed`, clearing `resolution` back to `NULL` (the
    /// "closed" reason no longer applies once state leaves `closed` — see
    /// `architecture.md`'s note that `resolution` only means something while
    /// `state == closed`). `assignee` is unchanged, and it decides the target
    /// state: `closed -> claimed` for an item that has one, `closed -> open`
    /// for one that never got claimed (closed straight from `open` by
    /// `remove`/`merge`/`force-close`). Landing the latter on `claimed` would
    /// strand it — `claimed` with a `NULL` assignee is a state nothing can
    /// leave, since `claim_item` needs `open`, `submit_item` needs a matching
    /// `assignee`, and `reject_item` needs `resolved`. Both targets are the
    /// assignee's turn per `turn_for`, so this picks a re-enterable state
    /// value without changing turn semantics. `reason` is required, recorded
    /// as an atomic comment, same pattern as `reject_item`. See ADR-0012.
    pub fn reopen_item(&self, id: &str, author: &str, reason: &str) -> Result<Item> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(StoreError::Validation(
                "reason must not be blank".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET
                 state = CASE WHEN assignee IS NULL THEN 'open' ELSE 'claimed' END,
                 resolution = NULL,
                 updated_at = ?1
             WHERE id = ?2 AND state = 'closed'",
            params![now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "reopen")?);
        }
        insert_lifecycle_comment(&conn, id, author, reason, now)?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically transitions `resolved -> closed` with `resolution = done`
    /// — the requester's approval. `author` is recorded as an atomic
    /// comment (traceability — see ADR-0012's "author" discussion).
    ///
    /// Only the item's `requester` may approve (mirrors `submit_item`'s
    /// `assignee` match on the other side of the handshake) — unless
    /// `requester` is unset, in which case there is no party to violate. See
    /// [ADR-0019](../../../docs/decisions/ADR-0019-approve-reject-requester-match.md).
    pub fn approve_item(&self, id: &str, author: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'closed', resolution = 'done', updated_at = ?1
             WHERE id = ?2 AND state = 'resolved' AND (requester IS NULL OR requester = ?3 COLLATE NOCASE)",
            params![now, id, author],
        )?;
        if affected == 0 {
            return Err(approve_reject_conflict(&conn, id, "approve", author)?);
        }
        insert_lifecycle_comment(&conn, id, author, "approved", now)?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Admin operation: closes an item created by mistake, with
    /// `resolution = invalid` ([architecture.md](../../../docs/architecture.md)'s
    /// admin-operation mapping). Unlike `approve_item`, this isn't gated on
    /// reaching `resolved` first — a mistaken item is caught at any
    /// pre-closed stage — and it's assignee-agnostic, matching `approve_item`.
    pub fn remove_item(&self, id: &str, author: &str) -> Result<Item> {
        self.close_with_resolution(id, Resolution::Invalid, "remove", author)
    }

    /// Admin operation: closes an item as a duplicate of another, with
    /// `resolution = duplicate`. Same any-pre-closed-state, assignee-agnostic
    /// rules as `remove_item` — see its doc comment — but unlike
    /// `remove_item`/`force_close_item` it doesn't go through
    /// `close_with_resolution`: `resolution = duplicate` alone can't say
    /// *duplicate of what*, so this also requires `duplicate_of_id` and
    /// atomically tags the item `duplicate-of:<id>` — a free-form-tag
    /// reference, not a new schema column, matching how tags stay opaque,
    /// caller-defined strings to the store (`principles.md` P-1). No
    /// referential check that `duplicate_of_id` names a real item — same
    /// accepted-consequence precedent as a dangling `related:<id>` tag
    /// surviving `delete_item`. See ADR-0015.
    pub fn merge_item(&self, id: &str, duplicate_of_id: &str, author: &str) -> Result<Item> {
        let duplicate_of_id = duplicate_of_id.trim();
        if duplicate_of_id.is_empty() {
            return Err(StoreError::Validation(
                "duplicate_of_id must not be blank".to_string(),
            ));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction()?;
        let resolved = resolve_item_id(&tx, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = tx.execute(
            "UPDATE items SET state = 'closed', resolution = 'duplicate', updated_at = ?1
             WHERE id = ?2 AND state != 'closed'",
            params![now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&tx, id, "merge")?);
        }
        insert_lifecycle_comment(&tx, id, author, "merge", now)?;
        tx.execute(
            "INSERT OR IGNORE INTO item_tags (item_id, tag) VALUES (?1, ?2)",
            params![id, format!("duplicate-of:{duplicate_of_id}")],
        )?;
        let item = row_to_item(&tx, id)?.ok_or(StoreError::NotFound)?;
        tx.commit()?;
        Ok(item)
    }

    /// Admin operation: closes an item that's become irrelevant, with
    /// `resolution = wontfix`. Same any-pre-closed-state, assignee-agnostic
    /// rules as `remove_item` — see its doc comment.
    pub fn force_close_item(&self, id: &str, author: &str) -> Result<Item> {
        self.close_with_resolution(id, Resolution::Wontfix, "force-close", author)
    }

    /// Admin operation: closes an item as done when the normal
    /// `claim → submit → approve` handshake never happened (e.g. a worker
    /// narrated completion entirely through comments and never called
    /// `claim_item`/`submit_item`), with `resolution = done`. Same
    /// any-pre-closed-state, assignee-agnostic rules as `remove_item` — see
    /// its doc comment. `resolution = done` alone can't distinguish this from
    /// a normal `approve_item`; the lifecycle comment's `"force-approve"` op
    /// name is what carries that distinction. See ADR-0017.
    pub fn force_approve_item(&self, id: &str, author: &str) -> Result<Item> {
        self.close_with_resolution(id, Resolution::Done, "force-approve", author)
    }

    fn close_with_resolution(
        &self,
        id: &str,
        resolution: Resolution,
        op: &str,
        author: &str,
    ) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'closed', resolution = ?1, updated_at = ?2
             WHERE id = ?3 AND state != 'closed'",
            params![resolution.as_str(), now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, op)?);
        }
        insert_lifecycle_comment(&conn, id, author, op, now)?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Same state-unrestricted shape as `close_with_resolution` (any
    /// pre-closed state, no assignee check), but the lifecycle comment
    /// carries the caller's free-text `reason` instead of a bare op-name
    /// marker — matching `reject_item`/`reopen_item`, not the admin closes.
    /// Backs `block_item`/`defer_item`: unlike `remove`/`merge`/
    /// `force-close`/`force-approve`, these are normal worker judgment
    /// calls (MCP-exposed), not admin overrides, so *why* is load-bearing
    /// for whoever later decides to `reopen_item`. See
    /// [ADR-0018](../../../docs/decisions/ADR-0018-blocked-deferred-resolution.md).
    fn close_with_reason(
        &self,
        id: &str,
        resolution: Resolution,
        op: &str,
        author: &str,
        reason: &str,
    ) -> Result<Item> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(StoreError::Validation(
                "reason must not be blank".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'closed', resolution = ?1, updated_at = ?2
             WHERE id = ?3 AND state != 'closed'",
            params![resolution.as_str(), now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, op)?);
        }
        insert_lifecycle_comment(&conn, id, author, reason, now)?;
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Parks an item that cannot progress right now because of a concrete
    /// external dependency (e.g. no access to a paywalled standard) —
    /// `resolution = blocked`. Callable by any worker on its own judgment
    /// from any pre-closed state (MCP-exposed, unlike the admin closes —
    /// see `architecture.md`'s MCP-exposure rule): fully reversible via
    /// `reopen_item` once the dependency clears, so it carries none of the
    /// self-approval risk `force-approve` guards against. `reason` is
    /// required — it is the only record of *why*, for whoever reopens
    /// later. See [ADR-0018](../../../docs/decisions/ADR-0018-blocked-deferred-resolution.md).
    pub fn block_item(&self, id: &str, author: &str, reason: &str) -> Result<Item> {
        self.close_with_reason(id, Resolution::Blocked, "block", author, reason)
    }

    /// Parks an item that is intentionally not being worked right now for a
    /// reason short of a hard external block (e.g. cross-consumer demand
    /// not yet proven) — `resolution = deferred`. Same shape and rationale
    /// as `block_item`; the two are separate resolutions rather than one
    /// with a flag because they are distinct dispositions a reader (or the
    /// console's badge) should be able to tell apart at a glance, the same
    /// way `duplicate`/`wontfix`/`invalid` already are. See
    /// [ADR-0018](../../../docs/decisions/ADR-0018-blocked-deferred-resolution.md).
    pub fn defer_item(&self, id: &str, author: &str, reason: &str) -> Result<Item> {
        self.close_with_reason(id, Resolution::Deferred, "defer", author, reason)
    }

    /// Adds `tags` to an item. Idempotent — already-present tags are
    /// silently skipped (`INSERT OR IGNORE`). Returns the item's full tag
    /// set after the add. Bumps `updated_at` only if a tag was actually
    /// new — a fully-idempotent call is not activity.
    pub fn add_tags(&self, item_id: &str, tags: &[String]) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, item_id)?;
        let item_id = resolved.as_str();
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
        let resolved = resolve_item_id(&conn, item_id)?;
        let item_id = resolved.as_str();
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

    /// Free-form `related:<id>` tag convention this parses — see
    /// [`RelatedItemRef`]. Deliberately not exposed as a public API on the
    /// tag vocabulary itself (P-1: tags stay opaque strings); this is the
    /// one place the core interprets this specific shape, and only for the
    /// read-only `related_items` helper below (docket-works#33).
    const RELATED_TAG_PREFIX: &str = "related:";

    /// Items linked to `id` via the `related:<id>` tag convention, both
    /// directions: this item's own tags naming another item
    /// (`RelatedRelation::References`), and other items' tags naming this
    /// one back (`RelatedRelation::ReferencedBy`). Best-effort — a
    /// `related:` tag whose target doesn't resolve to an existing item
    /// (typo, or the target was since deleted) is silently skipped rather
    /// than erroring, since this is a convenience view over free-form tags,
    /// not a referential-integrity check. See [`RelatedItemRef`].
    pub fn related_items(&self, id: &str) -> Result<Vec<RelatedItemRef>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let resolved = resolve_item_id(&conn, id)?;
        let id = resolved.as_str();
        require_item(&conn, id)?;

        let mut related = Vec::new();

        // references: parse this item's own tags for the `related:` prefix,
        // resolve each target (seq alias or bare id, same as any other
        // item-id-accepting call), and look up its seq/title. Both
        // resolution failure (e.g. a `related:` tag naming a seq that never
        // existed) and lookup failure (target id well-formed but no longer
        // present) are skipped, not propagated.
        for tag in tags_for_item(&conn, id)? {
            let Some(target) = tag.strip_prefix(Self::RELATED_TAG_PREFIX) else {
                continue;
            };
            let Ok(target_id) = resolve_item_id(&conn, target) else {
                continue;
            };
            if let Some((seq, title)) = fetch_seq_and_title(&conn, &target_id)? {
                related.push(RelatedItemRef {
                    id: target_id,
                    seq,
                    title,
                    relation: RelatedRelation::References,
                });
            }
        }

        // referenced_by: other items whose tags name this item's *canonical*
        // id exactly. A relating item that used the seq-alias form
        // (`related:#42`) instead of the canonical id isn't found here —
        // forward resolution above accepts both forms, but this reverse
        // lookup can only match the literal tag text stored on the other
        // item.
        let reverse_tag = format!("{}{id}", Self::RELATED_TAG_PREFIX);
        let mut stmt = conn.prepare(
            "SELECT items.id, items.seq, items.title FROM items
             JOIN item_tags ON items.id = item_tags.item_id
             WHERE item_tags.tag = ?1 AND items.id != ?2",
        )?;
        let rows = stmt.query_map(params![reverse_tag, id], |row| {
            Ok(RelatedItemRef {
                id: row.get(0)?,
                seq: row.get(1)?,
                title: row.get(2)?,
                relation: RelatedRelation::ReferencedBy,
            })
        })?;
        for row in rows {
            related.push(row?);
        }

        Ok(related)
    }

    /// The topic vocabulary, most-populated first — lets a caller discover
    /// which topics exist instead of enumerating candidate names one at a
    /// time (ADR-0014). Same `archived_at IS NULL` default as
    /// `list_items`/`search_items`; unlike those, there is no `archived`
    /// toggle here — a topic's existence in the vocabulary doesn't depend on
    /// whether every item under it happens to be archived.
    pub fn list_topics(&self) -> Result<Vec<TopicCount>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT topic, COUNT(*) FROM items WHERE archived_at IS NULL
             GROUP BY topic ORDER BY COUNT(*) DESC, topic ASC",
        )?;
        let rows = stmt.query_map([], topic_count_from_row)?;
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
        let resolved = resolve_item_id(&conn, item_id)?;
        let item_id = resolved.as_str();
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
        let resolved = resolve_item_id(&conn, item_id)?;
        let item_id = resolved.as_str();
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
    ///
    /// Seven independent, already-existing filter/sort dimensions — a
    /// params struct would be a separate refactor spanning `list_items` too
    /// (not scoped to ADR-0020, which only adds `order`), not a fix for
    /// this method alone.
    #[allow(clippy::too_many_arguments)]
    pub fn search_items(
        &self,
        topic: Option<&str>,
        state: Option<State>,
        tags: &[String],
        tag_match: TagMatch,
        query: Option<&str>,
        archived: Option<bool>,
        order: SortOrder,
    ) -> Result<Vec<Item>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut sql = String::from(
            "SELECT i.id, i.topic, i.title, i.body, i.state, i.resolution, i.requester, i.assignee, i.created_at, i.updated_at, i.archived_at, i.seq
             FROM items i WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(t) = topic {
            sql.push_str(" AND i.topic = ? COLLATE NOCASE");
            args.push(Box::new(t.to_string()));
        }
        if let Some(s) = state {
            sql.push_str(" AND i.state = ?");
            args.push(Box::new(s.as_str().to_string()));
        }
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
            let phrase = fts5_match_query(q);
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
        if archived.unwrap_or(false) {
            sql.push_str(" AND i.archived_at IS NOT NULL");
        } else {
            sql.push_str(" AND i.archived_at IS NULL");
        }
        sql.push_str(" ORDER BY i.updated_at ");
        sql.push_str(order.sql_keyword());

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

/// Builds an FTS5 `MATCH` argument out of a raw, untrusted search string.
///
/// FTS5 parses its own query syntax out of the raw string, so an ordinary
/// search term (`severity:medium`, `awaiting-release`, `@scope/name`) read
/// literally is a syntax error, not a search — quoting each word as its own
/// phrase literal (`"word"`) makes it match as plain text instead.
///
/// Quoting is done **per word**, not once around the whole query: a single
/// phrase literal around a multi-word query (the original shape here) means
/// "these words, adjacent, in this exact order" — which silently returns
/// nothing for any multi-word query that isn't already an exact contiguous
/// substring of the indexed text. Splitting on whitespace and
/// joining the per-word phrases with FTS5's default (implicit-AND) operator
/// asks for "all these words, anywhere" instead, which is what a search box
/// caller actually means. A trailing `*` prefix-matches each word, so a
/// query word also finds a token carrying a suffix `unicode61` (the default
/// tokenizer, no CJK segmentation) doesn't split off — e.g. a Korean
/// particle glued onto the noun the caller searched for.
fn fts5_match_query(q: &str) -> String {
    q.split_whitespace()
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
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

/// Upgrades a database created before [ADR-0013](../../../docs/decisions/ADR-0013-item-archive-and-delete.md):
/// adds the nullable `items.archived_at` column. `CREATE TABLE IF NOT EXISTS` above already
/// gives a fresh database this column directly, so this only fires for a database that
/// predates it — checked via `pragma_table_info`, same pattern as
/// `migrate_owner_to_requester_assignee`. Idempotent.
fn migrate_add_archived_at(conn: &Connection) -> rusqlite::Result<()> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('items') WHERE name = 'archived_at'")?
        .exists([])?;
    if !has_column {
        conn.execute_batch("ALTER TABLE items ADD COLUMN archived_at INTEGER;")?;
    }
    Ok(())
}

/// Upgrades a database created before [ADR-0016](../../../docs/decisions/ADR-0016-item-seq-alias.md):
/// adds the nullable `items.seq` column and backfills it in creation order
/// (`created_at` ASC, `id` ASC as a deterministic tiebreak for rows sharing a
/// millisecond) so existing items get the same stable, never-reused numbering
/// a fresh database assigns at `create_item` time. Seeds `seq_counter` to
/// continue from the highest assigned value. Same `pragma_table_info`-gated,
/// idempotent shape as `migrate_add_archived_at`.
fn migrate_add_item_seq(conn: &Connection) -> rusqlite::Result<()> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('items') WHERE name = 'seq'")?
        .exists([])?;
    if !has_column {
        conn.execute_batch("ALTER TABLE items ADD COLUMN seq INTEGER;")?;
        conn.execute(
            "UPDATE items SET seq = (
                 SELECT COUNT(*) FROM items i2
                 WHERE i2.created_at < items.created_at
                    OR (i2.created_at = items.created_at AND i2.id < items.id)
             ) + 1",
            [],
        )?;
        conn.execute(
            "INSERT INTO seq_counter (id, next) VALUES (1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM items))
             ON CONFLICT(id) DO UPDATE SET next = excluded.next",
            [],
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

/// Same shape as [`existing_state_conflict`], but for `approve_item`/
/// `reject_item`, whose `WHERE` clause can fail for either of two reasons —
/// wrong state, or a `requester` mismatch (ADR-0019) — that a caller needs to
/// tell apart: a wrong-state conflict is a stale-state retry, a requester
/// mismatch is an authorization violation (or a drifted identity fixable via
/// `set_item_requester`).
fn approve_reject_conflict(
    conn: &Connection,
    id: &str,
    op: &str,
    author: &str,
) -> Result<StoreError> {
    match row_to_item(conn, id)? {
        Some(item) => {
            if item.state != State::Resolved {
                Ok(StoreError::Conflict(format!(
                    "cannot {op}: item is {}",
                    item.state.as_str()
                )))
            } else {
                let requester = item.requester.as_deref().unwrap_or("(unset)");
                Ok(StoreError::Conflict(format!(
                    "cannot {op}: caller `{author}` does not match item's requester `{requester}` \
                     — if this is a drifted identity rather than a genuine wrong-party call, use \
                     set_item_requester to correct it"
                )))
            }
        }
        None => Ok(StoreError::NotFound),
    }
}

/// Resolves a caller-supplied item identifier to the canonical UUID. Accepts
/// either the UUID itself or the item's short numeric alias (`seq`), with or
/// without a leading `#` (`"142"` and `"#142"` both work) — see
/// [ADR-0016](../../../docs/decisions/ADR-0016-item-seq-alias.md). A UUID
/// never parses as a bare integer, so the two formats can't collide; anything
/// that doesn't parse as one is passed through unchanged, exactly today's
/// behavior for every existing caller. Every public `Store` method that takes
/// an item id calls this first, so callers below it always see a UUID.
fn resolve_item_id(conn: &Connection, id: &str) -> Result<String> {
    let candidate = id.strip_prefix('#').unwrap_or(id);
    match candidate.parse::<i64>() {
        Ok(seq) => conn
            .query_row("SELECT id FROM items WHERE seq = ?1", params![seq], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound),
        Err(_) => Ok(id.to_string()),
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

/// Records `author`/`body` as an atomic comment on `item_id`, sharing the
/// same `now` as the `UPDATE` it's paired with — the operations that call
/// this write the comment as part of the same change, not as separate
/// activity. Shared by `reject_item`/`reopen_item`/`approve_item`/
/// `close_with_resolution`/`set_item_requester`.
fn insert_lifecycle_comment(
    conn: &Connection,
    item_id: &str,
    author: &str,
    body: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let comment_id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO item_comments (id, item_id, author, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![comment_id, item_id, author, body, now],
    )?;
    Ok(())
}

fn row_to_item(conn: &Connection, id: &str) -> Result<Option<Item>> {
    let item = conn
        .query_row(
            "SELECT id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at, archived_at, seq
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
/// requester, assignee, created_at, updated_at, archived_at, seq`.
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
        open: Item::is_open(state),
        tags: Vec::new(),
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        archived_at: row.get(10)?,
        seq: row.get(11)?,
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

/// `None` if `id` doesn't exist — used by `related_items` to silently skip
/// a `related:` tag whose target no longer resolves to a real item.
fn fetch_seq_and_title(conn: &Connection, id: &str) -> rusqlite::Result<Option<(i64, String)>> {
    conn.query_row(
        "SELECT seq, title FROM items WHERE id = ?1",
        params![id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

fn tag_count_from_row(row: &rusqlite::Row) -> rusqlite::Result<TagCount> {
    Ok(TagCount {
        tag: row.get(0)?,
        count: row.get(1)?,
    })
}

fn topic_count_from_row(row: &rusqlite::Row) -> rusqlite::Result<TopicCount> {
    Ok(TopicCount {
        topic: row.get(0)?,
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

        let resolved = store.submit_item(&item.id, "w1", None).unwrap();
        assert_eq!(resolved.state, State::Resolved);

        let closed = store.approve_item(&item.id, "requester-1").unwrap();
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

        let closed = store.remove_item(&item.id, "admin").unwrap();
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

        let closed = store
            .merge_item(&item.id, "original-item-id", "admin")
            .unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Duplicate));
        // owner-agnostic — the claim it interrupted is preserved as history
        assert_eq!(closed.assignee.as_deref(), Some("w1"));
        assert!(
            closed
                .tags
                .iter()
                .any(|t| t == "duplicate-of:original-item-id")
        );
    }

    #[test]
    fn merge_item_rejects_blank_duplicate_of_id() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();

        let err = store.merge_item(&item.id, "   ", "admin").unwrap_err();
        assert!(matches!(err, StoreError::Validation(_)));
        // Rejected before the state-changing UPDATE runs — the item is
        // untouched, not left half-transitioned.
        assert_eq!(store.get_item(&item.id).unwrap().state, State::Open);
    }

    #[test]
    fn reject_resolved_item_returns_to_claimed_same_assignee() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let rejected = store
            .reject_item(&item.id, "requester-1", "not done yet, missing tests")
            .unwrap();
        assert_eq!(rejected.state, State::Claimed);
        assert_eq!(rejected.assignee.as_deref(), Some("w1"));
        assert!(rejected.open);
        assert_eq!(rejected.turn, Item::turn_for(State::Claimed));

        let comments = store.list_comments(&item.id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "requester-1");
        assert_eq!(comments[0].body, "not done yet, missing tests");
    }

    #[test]
    fn reject_non_resolved_item_conflicts() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        // still open, never claimed/submitted
        let err = store
            .reject_item(&item.id, "requester-1", "reason")
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[test]
    fn reject_with_blank_reason_is_invalid() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let err = store
            .reject_item(&item.id, "requester-1", "   ")
            .unwrap_err();
        assert!(matches!(err, StoreError::Validation(_)));
    }

    /// ADR-0019: `approve`/`reject` mirror `submit_item`'s `assignee` match
    /// on the requester side. `author` mismatching a set `requester` must
    /// fail, distinguishably from a plain wrong-state conflict.
    #[test]
    fn approve_rejects_when_author_does_not_match_requester() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], Some("requester-1"))
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let err = store.approve_item(&item.id, "someone-else").unwrap_err();
        let StoreError::Conflict(msg) = &err else {
            panic!("expected Conflict, got {err:?}");
        };
        assert!(msg.contains("someone-else"));
        assert!(msg.contains("requester-1"));
        assert_eq!(store.get_item(&item.id).unwrap().state, State::Resolved);
    }

    #[test]
    fn approve_succeeds_when_author_matches_requester() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], Some("requester-1"))
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let closed = store.approve_item(&item.id, "requester-1").unwrap();
        assert_eq!(closed.state, State::Closed);
    }

    /// No `requester` was ever named to hold this turn, so there is nothing
    /// for any `author` to violate — matches `submit_item`'s behavior when
    /// there is no comparable gap in its own model.
    #[test]
    fn approve_passes_through_when_requester_is_unset() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let closed = store.approve_item(&item.id, "anyone-at-all").unwrap();
        assert_eq!(closed.state, State::Closed);
    }

    #[test]
    fn reject_rejects_when_author_does_not_match_requester() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], Some("requester-1"))
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let err = store
            .reject_item(&item.id, "someone-else", "reason")
            .unwrap_err();
        let StoreError::Conflict(msg) = &err else {
            panic!("expected Conflict, got {err:?}");
        };
        assert!(msg.contains("someone-else"));
        assert!(msg.contains("requester-1"));
        assert_eq!(store.get_item(&item.id).unwrap().state, State::Resolved);
    }

    #[test]
    fn reject_succeeds_when_author_matches_requester() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], Some("requester-1"))
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let rejected = store
            .reject_item(&item.id, "requester-1", "not done yet")
            .unwrap();
        assert_eq!(rejected.state, State::Claimed);
    }

    #[test]
    fn reject_passes_through_when_requester_is_unset() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let rejected = store
            .reject_item(&item.id, "anyone-at-all", "not done yet")
            .unwrap();
        assert_eq!(rejected.state, State::Claimed);
    }

    #[test]
    fn reopen_closed_item_returns_to_claimed_and_clears_resolution() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();
        store.approve_item(&item.id, "requester-1").unwrap();

        let reopened = store
            .reopen_item(
                &item.id,
                "requester-1",
                "regression found, not actually fixed",
            )
            .unwrap();
        assert_eq!(reopened.state, State::Claimed);
        assert_eq!(reopened.resolution, None);
        assert_eq!(reopened.assignee.as_deref(), Some("w1"));
        assert!(reopened.open);

        let comments = store.list_comments(&item.id).unwrap();
        // one from approve_item's author record, one from reopen_item's reason
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].body, "regression found, not actually fixed");
    }

    /// An item closed straight from `open` was never claimed, so it has no
    /// assignee to send it back to — reopening it must land on `open`, not
    /// `claimed`. `claimed` with a `NULL` assignee is unrecoverable: nothing
    /// can claim it (needs `open`), submit it (needs a matching `assignee`),
    /// or reject it (needs `resolved`). See ADR-0012's 2026-08-20 update.
    #[test]
    fn reopen_never_claimed_item_returns_to_open_not_claimed() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        // Closed while still `open` — nobody ever claimed it.
        let closed = store.force_close_item(&item.id, "admin").unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.assignee, None);

        let reopened = store
            .reopen_item(&item.id, "requester-1", "closed prematurely")
            .unwrap();
        assert_eq!(reopened.state, State::Open);
        assert_eq!(reopened.assignee, None);
        assert_eq!(reopened.resolution, None);
        assert!(reopened.open);

        // And the state it landed on is actually re-enterable — the whole
        // point of the fix.
        let claimed = store.claim_item(&item.id, "w1").unwrap();
        assert_eq!(claimed.state, State::Claimed);
        assert_eq!(claimed.assignee.as_deref(), Some("w1"));
    }

    #[test]
    fn reopen_non_closed_item_conflicts() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        let err = store
            .reopen_item(&item.id, "requester-1", "reason")
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[test]
    fn reopen_with_blank_reason_is_invalid() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();
        store.approve_item(&item.id, "requester-1").unwrap();

        let err = store.reopen_item(&item.id, "requester-1", "").unwrap_err();
        assert!(matches!(err, StoreError::Validation(_)));
    }

    #[test]
    fn force_close_item_closes_a_resolved_item_as_wontfix() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        store.submit_item(&item.id, "w1", None).unwrap();

        let closed = store.force_close_item(&item.id, "admin").unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Wontfix));
    }

    #[test]
    fn force_approve_item_closes_an_open_item_never_claimed_or_submitted() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(item.state, State::Open);
        assert_eq!(item.assignee, None);

        let closed = store.force_approve_item(&item.id, "admin").unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Done));
        // never claimed — force-approve doesn't require or touch assignee
        assert_eq!(closed.assignee, None);
    }

    #[test]
    fn block_item_closes_an_open_item_as_blocked_and_records_reason() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();

        let closed = store
            .block_item(&item.id, "w1", "no access to the primary standard")
            .unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Blocked));
        assert_eq!(closed.turn, None);

        let comments = store.list_comments(&item.id).unwrap();
        assert_eq!(
            comments.last().unwrap().body,
            "no access to the primary standard"
        );
    }

    #[test]
    fn block_item_rejects_blank_reason() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();

        let err = store.block_item(&item.id, "w1", "  ").unwrap_err();
        assert!(matches!(err, StoreError::Validation(_)));
        assert_eq!(store.get_item(&item.id).unwrap().state, State::Open);
    }

    #[test]
    fn defer_item_closes_a_claimed_item_as_deferred_keeping_assignee() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();

        let closed = store
            .defer_item(&item.id, "w1", "cross-consumer demand not yet proven")
            .unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Deferred));
        assert_eq!(closed.assignee.as_deref(), Some("w1"));
    }

    #[test]
    fn reopen_item_reverses_block_and_defer_the_same_as_admin_closes() {
        let store = open_test_store();
        let blocked = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store
            .block_item(&blocked.id, "w1", "external dependency")
            .unwrap();
        let reopened = store
            .reopen_item(&blocked.id, "w1", "dependency resolved")
            .unwrap();
        assert_eq!(reopened.state, State::Open);
        assert_eq!(reopened.resolution, None);

        let deferred = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&deferred.id, "w2").unwrap();
        store
            .defer_item(&deferred.id, "w2", "not a priority yet")
            .unwrap();
        let reopened = store
            .reopen_item(&deferred.id, "w2", "second consumer showed up")
            .unwrap();
        assert_eq!(reopened.state, State::Claimed);
        assert_eq!(reopened.resolution, None);
    }

    #[test]
    fn admin_close_ops_reject_an_already_closed_item() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.remove_item(&item.id, "admin").unwrap();

        assert!(matches!(
            store
                .merge_item(&item.id, "original-item-id", "admin")
                .unwrap_err(),
            StoreError::Conflict(_)
        ));
        assert!(matches!(
            store.force_close_item(&item.id, "admin").unwrap_err(),
            StoreError::Conflict(_)
        ));
        assert!(matches!(
            store.force_approve_item(&item.id, "admin").unwrap_err(),
            StoreError::Conflict(_)
        ));
        assert!(matches!(
            store.remove_item(&item.id, "admin").unwrap_err(),
            StoreError::Conflict(_)
        ));
    }

    #[test]
    fn admin_close_ops_report_not_found_for_a_missing_item() {
        let store = open_test_store();
        assert!(matches!(
            store.remove_item("nope", "admin").unwrap_err(),
            StoreError::NotFound
        ));
        assert!(matches!(
            store
                .merge_item("nope", "original-item-id", "admin")
                .unwrap_err(),
            StoreError::NotFound
        ));
        assert!(matches!(
            store.force_close_item("nope", "admin").unwrap_err(),
            StoreError::NotFound
        ));
        assert!(matches!(
            store.force_approve_item("nope", "admin").unwrap_err(),
            StoreError::NotFound
        ));
    }

    #[test]
    fn approve_remove_merge_force_close_record_author_as_comment() {
        let store = open_test_store();

        let a = store
            .create_item("iyulab/docket", "a", None, &[], None)
            .unwrap();
        store.claim_item(&a.id, "w1").unwrap();
        store.submit_item(&a.id, "w1", None).unwrap();
        store.approve_item(&a.id, "requester-1").unwrap();
        assert_eq!(store.list_comments(&a.id).unwrap()[0].author, "requester-1");

        let b = store
            .create_item("iyulab/docket", "b", None, &[], None)
            .unwrap();
        store.remove_item(&b.id, "admin-1").unwrap();
        assert_eq!(store.list_comments(&b.id).unwrap()[0].author, "admin-1");
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
        let err = store.submit_item(&item.id, "w2", None).unwrap_err();
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
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("hydration"),
                None,
                SortOrder::Desc,
            )
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

    /// A database predating ADR-0016 has no `seq` column at all. `open`
    /// backfills one in creation order — not insertion order into this test,
    /// which deliberately inserts out of `created_at` order to prove the
    /// backfill sorts by `created_at`, not by whatever rowid SQLite happened
    /// to assign.
    #[test]
    fn open_backfills_seq_in_creation_order_on_a_legacy_database() {
        let dir = temp_db_dir("seq-migration-test");
        let db_path = dir.join("pre-adr-0016.db");

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
                    requester TEXT,
                    assignee TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    archived_at INTEGER
                );",
            )
            .unwrap();
        // Inserted newest-first; `created_at` order is oldest-first.
        legacy
            .execute(
                "INSERT INTO items (id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at)
                 VALUES ('newest', 'iyulab/docket', 'newest', NULL, 'open', NULL, NULL, NULL, 200, 200)",
                [],
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO items (id, topic, title, body, state, resolution, requester, assignee, created_at, updated_at)
                 VALUES ('oldest', 'iyulab/docket', 'oldest', NULL, 'open', NULL, NULL, NULL, 100, 100)",
                [],
            )
            .unwrap();
        drop(legacy);

        let store = Store::open(db_path.to_str().unwrap()).unwrap();
        assert_eq!(store.get_item("oldest").unwrap().seq, 1);
        assert_eq!(store.get_item("newest").unwrap().seq, 2);

        // The counter continues from the backfilled high-water mark, not
        // from 1 — a fresh item must not collide with a backfilled seq.
        let fresh = store
            .create_item("iyulab/docket", "fresh", None, &[], None)
            .unwrap();
        assert_eq!(fresh.seq, 3);

        // Idempotent: re-opening must not re-run the backfill (there is no
        // longer a missing `seq` column to trigger it) or renumber anything.
        drop(store);
        let store = Store::open(db_path.to_str().unwrap()).unwrap();
        assert_eq!(store.get_item("oldest").unwrap().seq, 1);

        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fresh_database_has_archived_at_column() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(item.archived_at, None);
    }

    #[test]
    fn migrate_add_archived_at_is_idempotent_on_a_fresh_database() {
        // Calling the migration a second time against an already-migrated
        // (or freshly created) database must not error.
        let conn = Connection::open(":memory:").unwrap();
        conn.execute_batch("CREATE TABLE items (id TEXT PRIMARY KEY, archived_at INTEGER);")
            .unwrap();
        migrate_add_archived_at(&conn).unwrap();
        migrate_add_archived_at(&conn).unwrap();
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

    /// `seq` is assigned from a single global counter (ADR-0016), so items
    /// created in sequence get consecutive numbers regardless of topic.
    #[test]
    fn create_item_assigns_sequential_seq_starting_at_one() {
        let store = open_test_store();
        let a = store
            .create_item("iyulab/docket", "a", None, &[], None)
            .unwrap();
        let b = store
            .create_item("iyulab/other-topic", "b", None, &[], None)
            .unwrap();
        let c = store
            .create_item("iyulab/docket", "c", None, &[], None)
            .unwrap();
        assert_eq!((a.seq, b.seq, c.seq), (1, 2, 3));
    }

    #[test]
    fn get_item_resolves_by_seq_with_and_without_hash_prefix() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(store.get_item(&item.seq.to_string()).unwrap().id, item.id);
        assert_eq!(
            store.get_item(&format!("#{}", item.seq)).unwrap().id,
            item.id
        );
        // The canonical UUID still works unchanged.
        assert_eq!(store.get_item(&item.id).unwrap().id, item.id);
    }

    #[test]
    fn get_item_by_unknown_seq_is_not_found() {
        let store = open_test_store();
        assert!(matches!(store.get_item("999"), Err(StoreError::NotFound)));
        assert!(matches!(store.get_item("#999"), Err(StoreError::NotFound)));
    }

    /// Every id-accepting operation, not just `get_item`, resolves a `seq`
    /// alias — spot-checked here on a state transition and a child-table
    /// write, the two shapes `resolve_item_id` is threaded into.
    #[test]
    fn claim_item_and_add_comment_accept_a_seq_alias() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        let alias = format!("#{}", item.seq);
        let claimed = store.claim_item(&alias, "w1").unwrap();
        assert_eq!(claimed.id, item.id);
        assert_eq!(claimed.state, State::Claimed);
        let comment = store.add_comment(&alias, "w1", "note").unwrap();
        assert_eq!(comment.item_id, item.id);
    }

    /// `seq` is never reused, even after the item it named is hard-deleted —
    /// a deleted item's number stays a permanent gap, matching how GitHub
    /// issue numbers behave. Guards against a rowid-reuse-style regression:
    /// `seq_counter` only ever advances, it isn't derived from `MAX(seq)` at
    /// allocation time.
    #[test]
    fn seq_is_never_reused_after_delete() {
        let store = open_test_store();
        let first = store
            .create_item("iyulab/docket", "first", None, &[], None)
            .unwrap();
        store.delete_item(&first.id).unwrap();
        let second = store
            .create_item("iyulab/docket", "second", None, &[], None)
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
    }

    /// `submit_item`'s `reason` is what distinguishes "done" from "I need a
    /// decision from you" — both are the same transition, because both are the
    /// same fact about whose turn it is (ADR-0010's 2026-09-08 update). Without
    /// a reason nothing is recorded, so an ordinary completed submission stays
    /// as quiet as it was before.
    /// ADR-0021: an identity that drifted in case is still the same identity,
    /// so the two-party handshake must not hard-fail on it. This is the exact
    /// shape observed in production — 21 items under `iyulab/Filer`, 2 under
    /// `iyulab/filer` — where the only way to approve was to pass the wrong
    /// spelling back (docket-works#37).
    #[test]
    fn approve_and_reject_match_a_requester_that_drifted_in_case() {
        let store = open_test_store();
        for (spelling, author) in [
            ("iyulab/Filer", "iyulab/filer"),
            ("iyulab/filer", "iyulab/Filer"),
        ] {
            let item = store
                .create_item("iyulab/docket", "t", None, &[], Some(spelling))
                .unwrap();
            store.claim_item(&item.id, "acme/worker").unwrap();
            store.submit_item(&item.id, "acme/worker", None).unwrap();
            let rejected = store
                .reject_item(&item.id, author, "one more pass")
                .unwrap();
            assert_eq!(rejected.state, State::Claimed);

            store.submit_item(&item.id, "acme/worker", None).unwrap();
            let approved = store.approve_item(&item.id, author).unwrap();
            assert_eq!(approved.state, State::Closed);
            // Storage is untouched by the comparison — the stored spelling
            // stays what was written.
            assert_eq!(approved.requester.as_deref(), Some(spelling));
        }
    }

    /// The assignee half of the same handshake — a second, independent drift
    /// (`iyu-devstack/Schemorph` vs `.../schemorph`) was found in the same audit.
    #[test]
    fn submit_matches_an_assignee_that_drifted_in_case() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store
            .claim_item(&item.id, "iyu-devstack/Schemorph")
            .unwrap();
        let submitted = store
            .submit_item(&item.id, "iyu-devstack/schemorph", None)
            .unwrap();
        assert_eq!(submitted.state, State::Resolved);
        assert_eq!(
            submitted.assignee.as_deref(),
            Some("iyu-devstack/Schemorph")
        );
    }

    /// `register_worker` is an every-session upsert, so a drifted id must land
    /// on the existing row rather than fail or fork it. The returned `id` is the
    /// canonical spelling — that response is how a caller learns its own id
    /// drifted. See ADR-0021's rejected 409 option.
    #[test]
    fn registering_a_case_variant_id_updates_the_existing_worker() {
        let store = open_test_store();
        store
            .register_worker("iyulab/Filer", &["iyulab/a".to_string()])
            .unwrap();

        let again = store
            .register_worker("iyulab/filer", &["iyulab/b".to_string()])
            .unwrap();
        assert_eq!(again.id, "iyulab/Filer", "canonical spelling is returned");
        assert_eq!(again.topics, vec!["iyulab/b".to_string()]);

        // One worker, not two — and reachable under either spelling.
        for spelling in ["iyulab/Filer", "iyulab/filer"] {
            let found = store.get_worker(spelling).unwrap();
            assert_eq!(found.id, "iyulab/Filer");
            assert_eq!(found.topics, vec!["iyulab/b".to_string()]);
        }
    }

    #[test]
    fn submit_item_records_a_reason_only_when_one_is_given() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], Some("acme/filer"))
            .unwrap();
        store.claim_item(&item.id, "acme/worker").unwrap();

        store.submit_item(&item.id, "acme/worker", None).unwrap();
        assert!(store.list_comments(&item.id).unwrap().is_empty());

        store
            .reject_item(&item.id, "acme/filer", "not yet")
            .unwrap();
        store
            .submit_item(
                &item.id,
                "acme/worker",
                Some("  which of the two schemas should this follow?  "),
            )
            .unwrap();

        let comments = store.list_comments(&item.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].author, "acme/worker");
        assert_eq!(
            comments[1].body,
            "which of the two schemas should this follow?"
        );

        // Whitespace-only counts as absent, same as every other optional
        // string in this crate.
        store
            .reject_item(&item.id, "acme/filer", "the first one")
            .unwrap();
        store
            .submit_item(&item.id, "acme/worker", Some("   "))
            .unwrap();
        assert_eq!(store.list_comments(&item.id).unwrap().len(), 3);
    }

    #[test]
    fn set_item_requester_updates_requester_and_bumps_updated_at() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(item.requester, None);
        let created_updated_at = item.updated_at;

        std::thread::sleep(std::time::Duration::from_millis(2));
        let updated = store
            .set_item_requester(&item.id, "admin", "  backfilled-reporter  ")
            .unwrap();
        assert_eq!(updated.requester.as_deref(), Some("backfilled-reporter"));
        assert!(updated.updated_at > created_updated_at);
    }

    /// Repairing an identity that is already set is the *other* half of what
    /// this operation is for, and the half ADR-0019 leans on: a drifted
    /// `requester` hard-fails a legitimate approve, and the only correct answer
    /// is to fix the item — not to approve under the wrong spelling. Pinned
    /// here because the tool description said "doesn't have one yet" long after
    /// the behavior and ADR-0019 both said otherwise, and a reader believed the
    /// description (docket-works#37).
    #[test]
    fn set_item_requester_repairs_an_identity_that_is_already_set() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], Some("acme/widget"))
            .unwrap();

        let updated = store
            .set_item_requester(&item.id, "acme/widget", "acme/Widget")
            .unwrap();
        assert_eq!(updated.requester.as_deref(), Some("acme/Widget"));
    }

    /// The correction is the one edit that can silently move an item between
    /// two parties, so it leaves a record naming both values — the same way
    /// every other why-bearing operation records its reason.
    #[test]
    fn set_item_requester_records_the_change_as_a_comment_and_no_ops_when_unchanged() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();

        store
            .set_item_requester(&item.id, "acme/fixer", "acme/widget")
            .unwrap();
        let comments = store.list_comments(&item.id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "acme/fixer");
        assert_eq!(comments[0].body, "requester: (unset) -> acme/widget");

        store
            .set_item_requester(&item.id, "acme/fixer", "acme/Widget")
            .unwrap();
        let comments = store.list_comments(&item.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].body, "requester: acme/widget -> acme/Widget");

        // Re-running the same correction adds nothing — a repeated call must
        // not fill the thread with noise.
        store
            .set_item_requester(&item.id, "acme/fixer", "acme/Widget")
            .unwrap();
        assert_eq!(store.list_comments(&item.id).unwrap().len(), 2);
    }

    #[test]
    fn set_item_requester_works_on_a_closed_item() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.remove_item(&item.id, "admin").unwrap();

        let updated = store
            .set_item_requester(&item.id, "admin", "reporter-1")
            .unwrap();
        assert_eq!(updated.state, State::Closed);
        assert_eq!(updated.requester.as_deref(), Some("reporter-1"));
    }

    #[test]
    fn set_item_requester_rejects_blank_and_missing_item() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert!(matches!(
            store.set_item_requester(&item.id, "admin", "   "),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            store.set_item_requester("nonexistent-id", "admin", "reporter-1"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn archive_item_sets_archived_at_regardless_of_state() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        assert_eq!(item.archived_at, None);

        let archived = store.archive_item(&item.id).unwrap();
        assert!(archived.archived_at.is_some());
        // archiving does not touch workflow state
        assert_eq!(archived.state, State::Open);
    }

    #[test]
    fn archive_item_is_idempotent() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        let first = store.archive_item(&item.id).unwrap();
        let second = store.archive_item(&item.id).unwrap();
        assert_eq!(first.archived_at, second.archived_at);
    }

    #[test]
    fn archive_nonexistent_item_is_not_found() {
        let store = open_test_store();
        let err = store.archive_item("does-not-exist").unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
    }

    #[test]
    fn delete_item_removes_item_and_cascades_tags_and_comments() {
        let store = open_test_store();
        let item = store
            .create_item(
                "iyulab/docket",
                "t",
                None,
                &["a".to_string(), "b".to_string()],
                None,
            )
            .unwrap();
        store
            .add_comment(&item.id, "author-1", "a comment")
            .unwrap();

        store.delete_item(&item.id).unwrap();

        assert!(matches!(
            store.get_item(&item.id).unwrap_err(),
            StoreError::NotFound
        ));
        // list_items over the whole store no longer includes it
        let all = store.list_items(None, None, None, SortOrder::Desc).unwrap();
        assert!(!all.iter().any(|i| i.id == item.id));

        // The cascade itself, not just what the public API surfaces —
        // orphaned rows in item_tags/item_comments would be invisible
        // through get_item/list_items but would still be there.
        let conn = store.conn.lock().unwrap();
        let tag_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM item_tags WHERE item_id = ?1",
                params![item.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag_count, 0);
        let comment_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM item_comments WHERE item_id = ?1",
                params![item.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(comment_count, 0);
    }

    #[test]
    fn delete_item_removes_it_from_full_text_search() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "unique-searchable-title", None, &[], None)
            .unwrap();
        let found_before = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("unique-searchable-title"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(found_before.len(), 1);

        store.delete_item(&item.id).unwrap();

        let found_after = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("unique-searchable-title"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert!(found_after.is_empty());
    }

    #[test]
    fn delete_item_works_regardless_of_state() {
        let store = open_test_store();
        let item = store
            .create_item("iyulab/docket", "t", None, &[], None)
            .unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        // still claimed, not closed — delete must still succeed
        store.delete_item(&item.id).unwrap();
        assert!(matches!(
            store.get_item(&item.id).unwrap_err(),
            StoreError::NotFound
        ));
    }

    #[test]
    fn delete_nonexistent_item_is_not_found() {
        let store = open_test_store();
        let err = store.delete_item("does-not-exist").unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
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

    /// docket-works#33: `related_items` resolves the forward direction
    /// (this item's own `related:` tags) whether the target is named by
    /// seq alias or canonical id, silently skips a tag whose target doesn't
    /// exist, and resolves the reverse direction (another item's tag naming
    /// this one back) — but *only* when that other tag used the canonical
    /// id, not a seq alias, matching the documented reverse-lookup
    /// limitation.
    #[test]
    fn related_items_resolves_both_directions_and_skips_dangling_references() {
        let store = open_test_store();
        let a = store
            .create_item("iyulab/docket", "a", None, &[], None)
            .unwrap();
        let b = store
            .create_item("iyulab/docket", "b", None, &[], None)
            .unwrap();
        let c = store
            .create_item("iyulab/docket", "c", None, &[], None)
            .unwrap();

        // a -> b via b's seq alias, a -> c via c's canonical id, plus a
        // dangling reference to an item that never existed.
        store
            .add_tags(
                &a.id,
                &[
                    format!("related:#{}", b.seq),
                    format!("related:{}", c.id),
                    "related:00000000-not-a-real-item".to_string(),
                ],
            )
            .unwrap();

        let from_a = store.related_items(&a.id).unwrap();
        assert_eq!(from_a.len(), 2, "dangling reference must be skipped");
        assert!(
            from_a
                .iter()
                .all(|r| r.relation == RelatedRelation::References)
        );
        let from_a_ids: Vec<&str> = from_a.iter().map(|r| r.id.as_str()).collect();
        assert!(from_a_ids.contains(&b.id.as_str()));
        assert!(from_a_ids.contains(&c.id.as_str()));

        // b was only referenced via its *seq alias* — the reverse lookup
        // matches literal tag text against the canonical id, so it does not
        // find this reference (documented limitation).
        assert!(store.related_items(&b.id).unwrap().is_empty());

        // c was referenced via its *canonical id* — the reverse lookup
        // finds it.
        let from_c = store.related_items(&c.id).unwrap();
        assert_eq!(from_c.len(), 1);
        assert_eq!(from_c[0].id, a.id);
        assert_eq!(from_c[0].title, "a");
        assert_eq!(from_c[0].relation, RelatedRelation::ReferencedBy);
    }

    #[test]
    fn related_items_on_an_item_with_no_related_tags_is_empty() {
        let store = open_test_store();
        let a = store
            .create_item("iyulab/docket", "lonely", None, &[], None)
            .unwrap();
        assert!(store.related_items(&a.id).unwrap().is_empty());
    }

    #[test]
    fn related_items_unknown_item_is_not_found() {
        let store = open_test_store();
        assert!(matches!(
            store.related_items("no-such-id"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn list_topics_counts_and_orders_by_frequency_then_excludes_archived() {
        let store = open_test_store();
        store
            .create_item("iyulab/node-packages", "a", None, &[], None)
            .unwrap();
        store
            .create_item("iyulab/node-packages", "b", None, &[], None)
            .unwrap();
        let c = store
            .create_item("iyulab/router", "c", None, &[], None)
            .unwrap();

        let topics = store.list_topics().unwrap();
        assert_eq!(topics.len(), 2);
        assert_eq!(topics[0].topic, "iyulab/node-packages");
        assert_eq!(topics[0].count, 2);
        assert_eq!(topics[1].topic, "iyulab/router");
        assert_eq!(topics[1].count, 1);

        // Archiving the router topic's only item drops it out of the
        // vocabulary count entirely — same default convention as
        // list_items/search_items (ADR-0014).
        store.archive_item(&c.id).unwrap();
        let topics = store.list_topics().unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].topic, "iyulab/node-packages");
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
                None,
                SortOrder::Desc,
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
                None,
                SortOrder::Desc,
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
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].topic, "iyulab/node-packages");
    }

    /// Regression test: a query whose words appear in the title but not
    /// contiguously in that exact order (a natural multi-word search, not
    /// a copy-pasted phrase) used to return nothing.
    #[test]
    fn search_items_query_matches_words_out_of_order() {
        let store = open_test_store();
        store
            .create_item(
                "iyulab/router",
                "generic EnumMember(Value=...) support for JsonOptions",
                None,
                &[],
                None,
            )
            .unwrap();

        let results = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("EnumMember JsonOptions"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    /// Regression test: a Korean multi-word query against a title carrying
    /// a trailing particle on the second word (`처리량이`, not `처리량`) —
    /// `unicode61` (the FTS5 default
    /// tokenizer) does no CJK segmentation, so an exact-token match on
    /// `처리량` alone would still miss it; this only passes with prefix
    /// matching per word.
    #[test]
    fn search_items_query_matches_korean_words_with_a_trailing_particle() {
        let store = open_test_store();
        store
            .create_item(
                "iyulab/shell-tunnel",
                "[shell-tunnel] 전송 처리량이 회선 대비 두 자릿수 낮게 관측됨",
                None,
                &[],
                None,
            )
            .unwrap();

        let results = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("전송 처리량"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    /// A query containing a literal `"` used to double-escape into a broken
    /// phrase that matched nothing, regardless of the rest of the query.
    #[test]
    fn search_items_query_with_a_literal_quote_still_matches() {
        let store = open_test_store();
        store
            .create_item("iyulab/docket", "quoted word test", None, &[], None)
            .unwrap();

        let results = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("\"quoted\" word"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    /// See ADR-0020: `order` picks the direction of the fixed `updated_at`
    /// sort, default `Desc` unchanged.
    #[test]
    fn list_items_order_asc_reverses_the_default_desc_ordering() {
        let store = open_test_store();
        let first = store
            .create_item("iyulab/docket", "first", None, &[], None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = store
            .create_item("iyulab/docket", "second", None, &[], None)
            .unwrap();

        let desc = store.list_items(None, None, None, SortOrder::Desc).unwrap();
        assert_eq!(desc[0].id, second.id);
        assert_eq!(desc[1].id, first.id);

        let asc = store.list_items(None, None, None, SortOrder::Asc).unwrap();
        assert_eq!(asc[0].id, first.id);
        assert_eq!(asc[1].id, second.id);
    }

    /// `search_items` gets the same `order` treatment as `list_items` — see
    /// ADR-0020.
    #[test]
    fn search_items_order_asc_reverses_the_default_desc_ordering() {
        let store = open_test_store();
        let first = store
            .create_item("iyulab/docket", "order-probe first", None, &[], None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = store
            .create_item("iyulab/docket", "order-probe second", None, &[], None)
            .unwrap();

        let desc = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("order-probe"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(desc[0].id, second.id);
        assert_eq!(desc[1].id, first.id);

        let asc = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("order-probe"),
                None,
                SortOrder::Asc,
            )
            .unwrap();
        assert_eq!(asc[0].id, first.id);
        assert_eq!(asc[1].id, second.id);
    }

    #[test]
    fn list_items_excludes_archived_by_default() {
        let store = open_test_store();
        let visible = store
            .create_item("iyulab/docket", "visible", None, &[], None)
            .unwrap();
        let hidden = store
            .create_item("iyulab/docket", "hidden", None, &[], None)
            .unwrap();
        store.archive_item(&hidden.id).unwrap();

        let default_view = store.list_items(None, None, None, SortOrder::Desc).unwrap();
        assert!(default_view.iter().any(|i| i.id == visible.id));
        assert!(!default_view.iter().any(|i| i.id == hidden.id));

        let archive_only = store
            .list_items(None, None, Some(true), SortOrder::Desc)
            .unwrap();
        assert!(!archive_only.iter().any(|i| i.id == visible.id));
        assert!(archive_only.iter().any(|i| i.id == hidden.id));
    }

    /// `search_items` takes the same `archived` parameter as `list_items`
    /// (both route through the same trailing `AND i.archived_at ...` clause
    /// in the SQL this builds) but only `list_items` had a test for it.
    #[test]
    fn search_items_archived_true_browses_only_the_archive() {
        let store = open_test_store();
        let visible = store
            .create_item("iyulab/docket", "matching visible", None, &[], None)
            .unwrap();
        let hidden = store
            .create_item("iyulab/docket", "matching hidden", None, &[], None)
            .unwrap();
        store.archive_item(&hidden.id).unwrap();

        let default_view = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("matching"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert!(default_view.iter().any(|i| i.id == visible.id));
        assert!(!default_view.iter().any(|i| i.id == hidden.id));

        let archive_only = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("matching"),
                Some(true),
                SortOrder::Desc,
            )
            .unwrap();
        assert!(!archive_only.iter().any(|i| i.id == visible.id));
        assert!(archive_only.iter().any(|i| i.id == hidden.id));
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
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("race in claim_item"),
                None,
                SortOrder::Desc,
            )
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
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("race in claim_item"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "legacy-1");

        // The AFTER INSERT trigger must also work going forward on a
        // backfilled table, not just the retroactive rebuild.
        store
            .add_comment("legacy-1", "requester", "thanks for the fix")
            .unwrap();
        let found2 = store
            .search_items(
                None,
                None,
                &[],
                TagMatch::Any,
                Some("thanks for the fix"),
                None,
                SortOrder::Desc,
            )
            .unwrap();
        assert_eq!(found2.len(), 1);

        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn put_alias_stores_and_lists_a_declared_variant() {
        let store = open_test_store();
        let a = store.put_alias("widget", "acme/widget").unwrap();
        assert_eq!(a.alias, "widget");
        assert_eq!(a.canonical, "acme/widget");
        let rows = store.list_aliases(None).unwrap();
        assert_eq!(rows.len(), 1);
        let filtered = store.list_aliases(Some("ACME/WIDGET")).unwrap();
        assert_eq!(filtered.len(), 1, "canonical filter folds case (ADR-0021)");
    }

    #[test]
    fn put_alias_is_idempotent_for_the_same_declaration() {
        let store = open_test_store();
        let first = store.put_alias("widget", "acme/widget").unwrap();
        let again = store.put_alias("WIDGET", "acme/Widget").unwrap();
        assert_eq!(
            again.created_at, first.created_at,
            "re-declaring changes nothing"
        );
        assert_eq!(again.alias, "widget", "the stored spelling wins");
        assert_eq!(again.canonical, "acme/widget", "the stored spelling wins");
        assert_eq!(store.list_aliases(None).unwrap().len(), 1);
    }

    #[test]
    fn put_alias_rejects_blank_and_self() {
        let store = open_test_store();
        assert!(matches!(
            store.put_alias("  ", "acme/widget"),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            store.put_alias("acme/widget", "ACME/WIDGET"),
            Err(StoreError::Validation(_))
        ));
    }

    #[test]
    fn put_alias_rejects_chains_in_both_directions() {
        let store = open_test_store();
        store.put_alias("widget", "acme/widget").unwrap();
        // Pointing a new alias *at* `widget` would build a two-hop chain
        // (w -> widget -> acme/widget), which the canonical-is-an-alias
        // check below rejects.
        assert!(matches!(
            store.put_alias("w", "widget"),
            Err(StoreError::Conflict(_))
        ));
        // And an existing canonical must not become an alias of something else.
        assert!(matches!(
            store.put_alias("acme/widget", "acme/gadget"),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn put_alias_rejects_repointing_an_existing_alias() {
        let store = open_test_store();
        store.put_alias("widget", "acme/widget").unwrap();
        assert!(matches!(
            store.put_alias("widget", "acme/gadget"),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn delete_alias_removes_it_and_404s_when_absent() {
        let store = open_test_store();
        store.put_alias("widget", "acme/widget").unwrap();
        store.delete_alias("WIDGET").unwrap();
        assert!(store.list_aliases(None).unwrap().is_empty());
        assert!(matches!(
            store.delete_alias("widget"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn alias_map_reflects_writes_and_deletes_without_reopening_the_store() {
        let store = open_test_store();
        assert!(store.alias_map().unwrap().is_empty());
        store.put_alias("widget", "acme/widget").unwrap();
        assert_eq!(store.alias_map().unwrap().resolve("widget"), "acme/widget");
        store.delete_alias("widget").unwrap();
        assert_eq!(
            store.alias_map().unwrap().resolve("widget"),
            "widget",
            "the cache is invalidated on delete, not just on insert"
        );
    }

    /// Regression for a race in `alias_map`'s cache-miss path: it drops
    /// `conn` after reading rows and only then takes the cache's write lock,
    /// leaving a window where a concurrent `put_alias`/`delete_alias` can
    /// invalidate in between — a loader that started before the mutation
    /// must not then store the snapshot it read beforehand over that
    /// invalidation, or the mutation becomes invisible until some later,
    /// unrelated write happens to invalidate again.
    ///
    /// This is expressed as concurrent writers plus concurrent readers
    /// rather than hand-driving the exact interleaving, because the race is
    /// in the gap between releasing one lock and acquiring another — there
    /// is no seam to pause it at from outside `alias_map`. It is not flaky:
    /// every assertion runs only after every spawned thread has joined, at
    /// which point no further mutation is in flight, so the final
    /// `alias_map()` call is a plain single-threaded read. What it exercises
    /// is that this call is guaranteed to observe every completed write
    /// regardless of how the concurrent calls interleaved — which holds only
    /// because a cached value's generation is checked against the current
    /// one, so a store built from a since-superseded read can never win a
    /// generation it no longer holds.
    #[test]
    fn concurrent_put_alias_and_alias_map_never_lose_a_write_to_a_stale_cache() {
        let store = Arc::new(open_test_store());
        let count = 32;
        let mut handles = Vec::new();
        for i in 0..count {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                store
                    .put_alias(&format!("widget{i}"), &format!("acme/widget{i}"))
                    .unwrap();
            }));
        }
        // Concurrent readers race the writers above; a reader's own result
        // is never checked, only that it can't corrupt the cache for the
        // assertions that run after every thread here has joined.
        for _ in 0..count {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                store.alias_map().unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let map = store.alias_map().unwrap();
        for i in 0..count {
            assert_eq!(
                map.resolve(&format!("widget{i}")),
                format!("acme/widget{i}"),
                "every completed write must be visible once all threads have joined"
            );
        }
    }
}
