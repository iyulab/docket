use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{Item, Resolution, State, Worker};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    /// The requested transition is not legal from the item's current state
    /// (e.g. claiming an already-claimed item, or an owner mismatch).
    #[error("conflict: {0}")]
    Conflict(String),
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
                owner TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_items_topic ON items(topic);
            CREATE INDEX IF NOT EXISTS idx_items_state ON items(state);",
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

    pub fn create_item(&self, topic: &str, title: &str, body: Option<&str>) -> Result<Item> {
        let id = Uuid::new_v4().to_string();
        let now = now_millis();
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO items (id, topic, title, body, state, resolution, owner, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'open', NULL, NULL, ?5, ?5)",
            params![id, topic, title, body, now],
        )?;
        Ok(Item {
            id,
            topic: topic.to_string(),
            title: title.to_string(),
            body: body.map(str::to_string),
            state: State::Open,
            resolution: None,
            owner: None,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn get_item(&self, id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Lists items, most recently updated first, optionally filtered by
    /// topic (exact match — prefix matching is a worker-side concern, see
    /// [`crate::domain::topic_matches`]) and/or state.
    pub fn list_items(&self, topic: Option<&str>, state: Option<State>) -> Result<Vec<Item>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut sql = String::from(
            "SELECT id, topic, title, body, state, resolution, owner, created_at, updated_at FROM items WHERE 1=1",
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
            (Some(t), Some(s)) => stmt.query_map(params![t, s.as_str()], item_from_row)?,
            (Some(t), None) => stmt.query_map(params![t], item_from_row)?,
            (None, Some(s)) => stmt.query_map(params![s.as_str()], item_from_row)?,
            (None, None) => stmt.query_map(params![], item_from_row)?,
        };
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Atomically transitions `open -> claimed` for `worker_id`. Fails with
    /// [`StoreError::Conflict`] if the item was not `open` (already claimed,
    /// or in a later state) — the case this exists to make exclusive.
    pub fn claim_item(&self, id: &str, worker_id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'claimed', owner = ?1, updated_at = ?2
             WHERE id = ?3 AND state = 'open'",
            params![worker_id, now, id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "claim")?);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically transitions `claimed -> resolved`. Only the current owner
    /// may submit.
    pub fn submit_item(&self, id: &str, worker_id: &str) -> Result<Item> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let now = now_millis();
        let affected = conn.execute(
            "UPDATE items SET state = 'resolved', updated_at = ?1
             WHERE id = ?2 AND state = 'claimed' AND owner = ?3",
            params![now, id, worker_id],
        )?;
        if affected == 0 {
            return Err(existing_state_conflict(&conn, id, "submit")?);
        }
        row_to_item(&conn, id)?.ok_or(StoreError::NotFound)
    }

    /// Atomically transitions `resolved -> closed` with `resolution = done`
    /// — the requester's approval. Other resolutions (duplicate/wontfix/
    /// invalid) are admin operations, not yet exposed (M3).
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

fn row_to_item(conn: &Connection, id: &str) -> Result<Option<Item>> {
    conn.query_row(
        "SELECT id, topic, title, body, state, resolution, owner, created_at, updated_at
         FROM items WHERE id = ?1",
        params![id],
        item_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn item_from_row(row: &rusqlite::Row) -> rusqlite::Result<Item> {
    let state_str: String = row.get(4)?;
    let resolution_str: Option<String> = row.get(5)?;
    Ok(Item {
        id: row.get(0)?,
        topic: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        state: State::parse(&state_str).expect("state column always holds a valid State"),
        resolution: resolution_str.and_then(|s| Resolution::parse(&s)),
        owner: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
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
        let item = store.create_item("iyulab/docket", "fix bug", None).unwrap();
        assert_eq!(item.state, State::Open);

        let claimed = store.claim_item(&item.id, "w1").unwrap();
        assert_eq!(claimed.state, State::Claimed);
        assert_eq!(claimed.owner.as_deref(), Some("w1"));

        let resolved = store.submit_item(&item.id, "w1").unwrap();
        assert_eq!(resolved.state, State::Resolved);

        let closed = store.approve_item(&item.id).unwrap();
        assert_eq!(closed.state, State::Closed);
        assert_eq!(closed.resolution, Some(Resolution::Done));
    }

    #[test]
    fn claim_rejects_already_claimed() {
        let store = open_test_store();
        let item = store.create_item("iyulab/docket", "t", None).unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        let err = store.claim_item(&item.id, "w2").unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[test]
    fn submit_rejects_non_owner() {
        let store = open_test_store();
        let item = store.create_item("iyulab/docket", "t", None).unwrap();
        store.claim_item(&item.id, "w1").unwrap();
        let err = store.submit_item(&item.id, "w2").unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    /// The M1 completion criterion: if two workers race to claim the same
    /// item, exactly one succeeds.
    #[test]
    fn concurrent_claims_exactly_one_winner() {
        let store = Arc::new(open_test_store());
        let item = store.create_item("iyulab/docket", "race", None).unwrap();

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
}
