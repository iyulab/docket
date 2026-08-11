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

/// A single unit of work waiting to be processed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub topic: String,
    pub title: String,
    pub body: Option<String>,
    pub state: State,
    pub resolution: Option<Resolution>,
    pub owner: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
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
}
