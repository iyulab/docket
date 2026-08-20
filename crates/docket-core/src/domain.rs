use serde::{Deserialize, Serialize};

/// Workflow stage of an item. See docs/architecture.md "Item state schema".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Claimed,
    Resolved,
    Closed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Claimed => "claimed",
            State::Resolved => "resolved",
            State::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(State::Open),
            "claimed" => Some(State::Claimed),
            "resolved" => Some(State::Resolved),
            "closed" => Some(State::Closed),
            _ => None,
        }
    }
}

/// Why an item was closed. Only meaningful once `state == Closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Done,
    Duplicate,
    Wontfix,
    Invalid,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Done => "done",
            Resolution::Duplicate => "duplicate",
            Resolution::Wontfix => "wontfix",
            Resolution::Invalid => "invalid",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "done" => Some(Resolution::Done),
            "duplicate" => Some(Resolution::Duplicate),
            "wontfix" => Some(Resolution::Wontfix),
            "invalid" => Some(Resolution::Invalid),
            _ => None,
        }
    }
}

/// An entity that can process work. The core does not know whether it is a
/// human, an AI session, or a script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub id: String,
    /// Topic prefixes this worker owns (see [`topic_matches`]).
    pub topics: Vec<String>,
    pub online: bool,
}

/// Whose hand an item is currently in. Derived from `state` — see
/// [`Item::turn_for`] — and never persisted, so it can't drift out of sync
/// with the state it's computed from (see [ADR-0011](../../../docs/decisions/ADR-0011-requester-assignee-naming.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Turn {
    Requester,
    Assignee,
}

/// A single unit of work waiting to be processed.
///
/// `requester`/`assignee` are the same name on the wire, in storage, and
/// here — no serde rename needed. See
/// [ADR-0011](../../../docs/decisions/ADR-0011-requester-assignee-naming.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub topic: String,
    pub title: String,
    pub body: Option<String>,
    pub state: State,
    pub resolution: Option<Resolution>,
    /// Who this item is being worked for. Optional, set at creation only.
    pub requester: Option<String>,
    /// The worker currently holding the item (was `owner`) — set by `claim`,
    /// checked by `submit`.
    pub assignee: Option<String>,
    /// Derived, not stored — see [`Item::turn_for`].
    pub turn: Option<Turn>,
    /// Derived, not stored — `state != Closed`. Same treatment as `turn`
    /// (ADR-0010) and for the same reason: fully determined by `state`, so
    /// storing it separately would be a second source of truth that could
    /// drift. See ADR-0012.
    pub open: bool,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Item {
    /// Whose turn it is, purely as a function of `state` — the single place
    /// this mapping is defined, so every code path that builds an `Item`
    /// (create/claim/submit/approve/list/search/...) stays consistent by
    /// construction rather than by convention.
    ///
    /// `open` reads as the assignee's turn, the same as `claimed` — an open
    /// item is unclaimed but still squarely waiting on whichever topic owns
    /// it to look at it and act, which is exactly what "turn" means for
    /// `claimed` too. Only `closed` is nobody's turn — see the 2026-08-18
    /// update in [ADR-0010](../../../docs/decisions/ADR-0010-item-from-to-turn.md).
    pub fn turn_for(state: State) -> Option<Turn> {
        match state {
            State::Open => Some(Turn::Assignee),
            State::Claimed => Some(Turn::Assignee),
            State::Resolved => Some(Turn::Requester),
            State::Closed => None,
        }
    }

    /// Whether the item is still "on the board" — the GitHub-style coarse
    /// view over the full `state` value. `false` only for `Closed`. See
    /// ADR-0012.
    pub fn is_open(state: State) -> bool {
        state != State::Closed
    }
}

/// Prefix match on `/`-separated topic paths: a worker owning `iyulab` is a
/// candidate for an item in front of `iyulab/docket`, but not `iyulab2/x`.
pub fn topic_matches(owned: &str, item_topic: &str) -> bool {
    if owned == item_topic {
        return true;
    }
    item_topic
        .strip_prefix(owned)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// How `search_items`'s `tags` filter combines multiple tags. See
/// docs/architecture.md — tags are opaque to the core, this only governs
/// set logic (does an item need ANY of the given tags, or ALL of them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagMatch {
    Any,
    All,
}

impl TagMatch {
    pub fn as_str(self) -> &'static str {
        match self {
            TagMatch::Any => "any",
            TagMatch::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "any" => Some(TagMatch::Any),
            "all" => Some(TagMatch::All),
            _ => None,
        }
    }
}

/// One row of `list_tags` — a tag and how many items currently carry it,
/// so a caller can browse existing vocabulary before inventing a new tag
/// string (avoids synonym drift, e.g. "release-pending" vs "awaiting-release").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

/// A single append-only note attached to an item. No edit/delete API by
/// design — corrections are new comments, matching the project's existing
/// "history isn't rewritten" convention for issue drafts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub item_id: String,
    pub author: String,
    pub body: String,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_prefix_matches_segment_boundary() {
        assert!(topic_matches("iyulab", "iyulab/docket"));
        assert!(topic_matches("iyulab/docket", "iyulab/docket"));
        assert!(!topic_matches("iyulab", "iyulab2/docket"));
        assert!(!topic_matches("iyulab/docket", "iyulab/dock"));
    }

    #[test]
    fn state_round_trips_through_str() {
        for s in [State::Open, State::Claimed, State::Resolved, State::Closed] {
            assert_eq!(State::parse(s.as_str()), Some(s));
        }
    }

    /// `open` and `claimed` both read as the assignee's turn — an unclaimed
    /// item is still waiting on the assignee side to look at it, same as a
    /// claimed one. Only `closed` is nobody's turn.
    #[test]
    fn turn_for_open_and_claimed_is_assignee_resolved_is_requester_closed_is_none() {
        assert_eq!(Item::turn_for(State::Open), Some(Turn::Assignee));
        assert_eq!(Item::turn_for(State::Claimed), Some(Turn::Assignee));
        assert_eq!(Item::turn_for(State::Resolved), Some(Turn::Requester));
        assert_eq!(Item::turn_for(State::Closed), None);
    }

    #[test]
    fn is_open_is_true_except_when_closed() {
        assert!(Item::is_open(State::Open));
        assert!(Item::is_open(State::Claimed));
        assert!(Item::is_open(State::Resolved));
        assert!(!Item::is_open(State::Closed));
    }

    #[test]
    fn tag_match_round_trips_through_str() {
        for m in [TagMatch::Any, TagMatch::All] {
            assert_eq!(TagMatch::parse(m.as_str()), Some(m));
        }
    }
}
