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
///
/// `Blocked`/`Deferred` are not admin overrides like the other four — they
/// are the normal, reversible way a worker parks an item that cannot
/// currently progress (an external dependency, unproven cross-consumer
/// demand). `reopen_item` is the way back for any of the six. See
/// [ADR-0018](../../../docs/decisions/ADR-0018-blocked-deferred-resolution.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Done,
    Duplicate,
    Wontfix,
    Invalid,
    Blocked,
    Deferred,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Done => "done",
            Resolution::Duplicate => "duplicate",
            Resolution::Wontfix => "wontfix",
            Resolution::Invalid => "invalid",
            Resolution::Blocked => "blocked",
            Resolution::Deferred => "deferred",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "done" => Some(Resolution::Done),
            "duplicate" => Some(Resolution::Duplicate),
            "wontfix" => Some(Resolution::Wontfix),
            "invalid" => Some(Resolution::Invalid),
            "blocked" => Some(Resolution::Blocked),
            "deferred" => Some(Resolution::Deferred),
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
    /// Short numeric alias for `id` — assigned once at creation, from a
    /// single global counter, never reused (even after delete). Every
    /// id-accepting operation takes either form interchangeably; `id` stays
    /// canonical. See [ADR-0016](../../../docs/decisions/ADR-0016-item-seq-alias.md).
    pub seq: i64,
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
    /// `None` unless archived. Independent of `state`/`open` — an
    /// archived item can be any workflow state. See ADR-0013.
    pub archived_at: Option<i64>,
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

/// Whether two identifiers name the same thing.
///
/// `worker id`, `requester`, `assignee` and `topic` exist only to be compared,
/// and two spellings differing in case are the same identity — see
/// [ADR-0021](../../../docs/decisions/ADR-0021-case-insensitive-identity.md).
/// Byte-exact comparison silently split one identity in two, which no query
/// anywhere reported.
///
/// **ASCII only.** `eq_ignore_ascii_case` leaves every non-ASCII codepoint
/// byte-exact, which covers the `org/repo` convention these follow entirely and
/// costs no allocation. Every site that compares an identifier routes through
/// here precisely so widening that to full Unicode stays a one-place change.
pub fn identity_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Whether `a` names the same thing as `b`, for optional identifiers — `None`
/// never matches, the same way a `NULL` column never matched an exact filter.
pub fn identity_eq_opt(a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| identity_eq(a, b))
}

/// Prefix match on `/`-separated topic paths: a worker owning `iyulab` is a
/// candidate for an item in front of `iyulab/docket`, but not `iyulab2/x`.
/// Case-insensitive on both arms, since a topic is an identifier like any other
/// (ADR-0021) — no drift has been observed here, this is consistency of the
/// class rather than a fix for something seen.
pub fn topic_matches(owned: &str, item_topic: &str) -> bool {
    if identity_eq(owned, item_topic) {
        return true;
    }
    item_topic
        .get(..owned.len())
        .is_some_and(|head| identity_eq(head, owned))
        && item_topic.as_bytes().get(owned.len()) == Some(&b'/')
}

/// Every declared alias, indexed for lookup. Built from the store's
/// `identity_aliases` rows; see [`Alias`] for the one-hop invariant this
/// relies on — because a chain can never exist, `resolve` is a single map
/// lookup and needs no loop or cycle guard.
#[derive(Debug, Clone, Default)]
pub struct AliasMap {
    /// lowercased alias -> canonical, as declared
    forward: std::collections::HashMap<String, String>,
    /// lowercased canonical -> its aliases, as declared
    reverse: std::collections::HashMap<String, Vec<String>>,
}

impl AliasMap {
    pub fn from_rows(rows: impl IntoIterator<Item = Alias>) -> Self {
        let mut map = Self::default();
        for row in rows {
            map.reverse
                .entry(row.canonical.to_ascii_lowercase())
                .or_default()
                .push(row.alias.clone());
            map.forward
                .insert(row.alias.to_ascii_lowercase(), row.canonical);
        }
        map
    }

    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// The canonical spelling of `id`. One hop — an id that is not a declared
    /// alias is its own canonical, which makes this total: there is no
    /// "unresolvable identifier" case for any caller to handle.
    pub fn resolve<'a>(&'a self, id: &'a str) -> &'a str {
        self.forward
            .get(&id.to_ascii_lowercase())
            .map(String::as_str)
            .unwrap_or(id)
    }

    /// `resolve(id)` plus every spelling declared for it — the set a SQL
    /// `IN (…)` has to cover to match the same rows the in-memory comparisons
    /// do. Always contains at least one element.
    pub fn group_of(&self, id: &str) -> Vec<String> {
        let canonical = self.resolve(id);
        let mut group = vec![canonical.to_string()];
        if let Some(aliases) = self.reverse.get(&canonical.to_ascii_lowercase()) {
            group.extend(aliases.iter().cloned());
        }
        group
    }
}

/// [`topic_matches`], folding declared aliases first (ADR-0022).
///
/// **Order matters**: resolve each whole identifier, *then* prefix-compare.
/// The other order breaks a worker registered on a bare prefix (`acme`)
/// reaching an item filed under an alias with no org segment (`widget`),
/// because the prefix test would run against the unresolved spelling.
///
/// Every item-matching site must use this, not [`topic_matches`] — the plain
/// form remains for comparing alias-table keys themselves, where folding
/// would be circular.
pub fn topic_matches_with(aliases: &AliasMap, owned: &str, item_topic: &str) -> bool {
    topic_matches(aliases.resolve(owned), aliases.resolve(item_topic))
}

/// [`identity_eq_opt`], folding declared aliases first (ADR-0022). Same
/// use-this-not-that rule as [`topic_matches_with`].
pub fn identity_eq_opt_with(aliases: &AliasMap, a: Option<&str>, b: &str) -> bool {
    a.is_some_and(|a| identity_eq(aliases.resolve(a), aliases.resolve(b)))
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

/// Sort direction for `list_items`/`search_items`' fixed `updated_at`
/// column — see [ADR-0020](../../../docs/decisions/ADR-0020-list-search-order-parameter.md).
/// `Desc` (most-recently-touched first) is the default, matching this
/// project's behavior before this parameter existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

impl SortOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            SortOrder::Asc => "asc",
            SortOrder::Desc => "desc",
        }
    }

    /// An unrecognized value is the caller's problem to notice via
    /// behavior, not a hard error — same treatment `TagMatch::parse`'s
    /// callers already give an unrecognized `tag_match` (falls back to the
    /// default rather than rejecting the request).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "asc" => Some(SortOrder::Asc),
            "desc" => Some(SortOrder::Desc),
            _ => None,
        }
    }

    /// The literal `ORDER BY` keyword — safe to interpolate directly since
    /// it's drawn from this closed enum, never from caller-supplied text.
    pub(crate) fn sql_keyword(self) -> &'static str {
        match self {
            SortOrder::Asc => "ASC",
            SortOrder::Desc => "DESC",
        }
    }
}

/// Which side of a `related:<id>` tag pairing this row represents — see
/// [`RelatedItemRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelatedRelation {
    /// The item being described carries a `related:<this row's id>` tag —
    /// it's pointing at this row.
    References,
    /// This row's item carries a `related:<the item being described's id>`
    /// tag — it's pointing back.
    ReferencedBy,
}

/// One item linked to another via the `related:<id>` free-form tag
/// convention (docket-works#33), derived at request time by
/// `Store::related_items` for `get_item(expand_related=true)` — not a
/// stored concept, not part of [`Item`]. The core still treats tags as
/// fully opaque strings (P-1); this is a best-effort helper that
/// interprets one specific tag shape on top, entirely at read time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedItemRef {
    pub id: String,
    pub seq: i64,
    pub title: String,
    pub relation: RelatedRelation,
}

/// One row of `list_tags` — a tag and how many items currently carry it,
/// so a caller can browse existing vocabulary before inventing a new tag
/// string (avoids synonym drift, e.g. "release-pending" vs "awaiting-release").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

/// One row of `list_topics` — a topic and how many non-archived items
/// currently sit under it, so a caller can discover the topic vocabulary
/// instead of guessing/enumerating candidate names. See
/// [ADR-0014](../../../docs/decisions/ADR-0014-list-search-pagination-and-list-topics.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicCount {
    /// The canonical spelling. Variants declared in `identity_aliases` fold
    /// into this row rather than standing as rows of their own (ADR-0022).
    pub topic: String,
    pub count: i64,
    /// Which declared spellings folded into this row. Empty for a topic with
    /// no aliases. Carried over the API so a client can surface what folded
    /// into this count — as of this writing no client renders it yet.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A declared spelling variant of one identity — `alias` names the same thing
/// as `canonical`. Resolution is always one hop: an `alias` may never itself be
/// another row's `canonical`, and a `canonical` may never be another row's
/// `alias` (enforced in `Store::put_alias`). See
/// [ADR-0022](../../../docs/decisions/ADR-0022-identity-alias.md) for why
/// chains are structurally forbidden rather than merely deferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alias {
    pub alias: String,
    pub canonical: String,
    pub created_at: i64,
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

/// One append-only row per state-affecting or discussion-affecting write —
/// `created`/`transition`/`comment`. Exists to answer "what happened since
/// I last looked" without touching `turn`: see ADR-0010's 2026-09-09
/// update for why `turn` itself must stay a workflow-ownership field, not
/// a read/unread one. `seq` is a dedicated monotonic counter
/// (`event_seq_counter`), never `item_comments.rowid` or `created_at` —
/// the same reasoning as `items.seq` (ADR-0016): rowid is reused after a
/// row is deleted, and epoch millis can collide, so either would let a
/// cursor skip an event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub seq: i64,
    pub item_id: String,
    /// One of `"created"` / `"transition"` / `"comment"` — not a Rust enum
    /// at this layer, same treatment `tag`/`comment` bodies get (ADR-0009):
    /// core doesn't need to branch on it, only store and return it.
    pub kind: String,
    pub actor: String,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0021. The fixtures use the two spellings actually observed drifting
    /// in the running dataset, not invented ones — a `w1`-shaped fixture cannot
    /// reproduce this class at all.
    #[test]
    fn identity_comparison_folds_ascii_case() {
        assert!(identity_eq("iyulab/Filer", "iyulab/filer"));
        assert!(identity_eq(
            "iyu-devstack/Schemorph",
            "iyu-devstack/schemorph"
        ));
        assert!(!identity_eq("iyulab/Filer", "iyulab/Filer2"));

        assert!(identity_eq_opt(Some("iyulab/Filer"), "iyulab/filer"));
        assert!(!identity_eq_opt(None, "iyulab/filer"));
    }

    /// Non-ASCII stays byte-exact — the documented limit of `identity_eq`, kept
    /// visible so widening it later is a deliberate change rather than a
    /// surprise about what was already covered.
    #[test]
    fn identity_comparison_does_not_fold_non_ascii() {
        assert!(!identity_eq("acme/gr\u{fc}n", "acme/GR\u{dc}N"));
    }

    #[test]
    fn topic_prefix_matches_ignoring_case_on_both_arms() {
        assert!(topic_matches("IYULAB", "iyulab/docket"));
        assert!(topic_matches("iyulab", "IYULAB/Docket"));
        assert!(topic_matches("iyulab/Docket", "iyulab/docket"));
        // Folding must not loosen the segment boundary the prefix match is for.
        assert!(!topic_matches("IYULAB", "iyulab2/docket"));
        assert!(!topic_matches("iyulab/Docket", "iyulab/dock"));
    }

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

    #[test]
    fn sort_order_round_trips_through_str_and_defaults_to_desc() {
        for o in [SortOrder::Asc, SortOrder::Desc] {
            assert_eq!(SortOrder::parse(o.as_str()), Some(o));
        }
        assert_eq!(SortOrder::default(), SortOrder::Desc);
        assert_eq!(SortOrder::parse("sideways"), None);
    }

    fn alias_row(alias: &str, canonical: &str) -> Alias {
        Alias {
            alias: alias.to_string(),
            canonical: canonical.to_string(),
            created_at: 0,
        }
    }

    #[test]
    fn resolve_folds_a_declared_variant_and_passes_unknowns_through() {
        let map = AliasMap::from_rows(vec![alias_row("widget", "acme/widget")]);
        assert_eq!(map.resolve("widget"), "acme/widget");
        assert_eq!(map.resolve("WIDGET"), "acme/widget", "lookup folds case");
        assert_eq!(
            map.resolve("acme/widget"),
            "acme/widget",
            "a canonical resolves to itself"
        );
        assert_eq!(
            map.resolve("acme/gadget"),
            "acme/gadget",
            "an unknown id is its own canonical"
        );
    }

    #[test]
    fn group_of_returns_the_canonical_and_every_declared_variant() {
        let map = AliasMap::from_rows(vec![
            alias_row("widget", "acme/widget"),
            alias_row("widget-rs", "acme/widget"),
            alias_row("gadget", "acme/gadget"),
        ]);
        let mut group = map.group_of("WIDGET-RS");
        group.sort();
        assert_eq!(group, vec!["acme/widget", "widget", "widget-rs"]);
        assert_eq!(
            map.group_of("acme/other"),
            vec!["acme/other"],
            "unknown ids group alone"
        );
    }

    #[test]
    fn topic_matches_with_resolves_before_prefix_comparing() {
        let map = AliasMap::from_rows(vec![alias_row("widget", "acme/widget")]);
        // A worker registered on the bare prefix `acme` must reach an item
        // filed under an alias that carries no org segment at all. This only
        // works if the whole identifier is resolved *first* and the prefix
        // comparison runs on the result.
        assert!(topic_matches_with(&map, "acme", "widget"));
        assert!(topic_matches_with(&map, "widget", "acme/widget"));
        assert!(!topic_matches_with(&map, "other", "widget"));
    }

    #[test]
    fn identity_eq_opt_with_folds_declared_variants() {
        let map = AliasMap::from_rows(vec![alias_row("widget", "acme/widget")]);
        assert!(identity_eq_opt_with(&map, Some("widget"), "ACME/Widget"));
        assert!(!identity_eq_opt_with(&map, None, "acme/widget"));
        assert!(!identity_eq_opt_with(
            &map,
            Some("acme/gadget"),
            "acme/widget"
        ));
    }
}
