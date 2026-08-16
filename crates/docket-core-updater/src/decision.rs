//! Pure decision of whether an update should run — kept separate from I/O
//! so it's exhaustively unit-testable without a mock server.

/// An update should run only when the running version differs from the
/// latest release *and* the server has been idle at least
/// `idle_threshold_secs`. Both conditions must hold — a version match
/// alone (nothing to update) or a fresh version with too little idle time
/// (would risk swapping mid-request) each skip the update on their own.
pub fn should_update(
    local_version: &str,
    remote_version: &str,
    idle_seconds: u64,
    idle_threshold_secs: u64,
) -> bool {
    local_version != remote_version && idle_seconds >= idle_threshold_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_version_never_updates_even_when_idle() {
        assert!(!should_update("v0.2.0", "v0.2.0", 10_000, 300));
    }

    #[test]
    fn different_version_and_enough_idle_updates() {
        assert!(should_update("v0.1.0", "v0.2.0", 300, 300));
    }

    #[test]
    fn different_version_but_not_idle_enough_skips() {
        assert!(!should_update("v0.1.0", "v0.2.0", 299, 300));
    }

    #[test]
    fn idle_seconds_exactly_at_threshold_updates() {
        assert!(should_update("v0.1.0", "v0.2.0", 300, 300));
    }
}
