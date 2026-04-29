//! In-memory ranker.
//!
//! v1.0 = prefix match + per-(spec, prefix) usage counter. No SQLite,
//! no frecency decay. See PLAN.md §7. Fuzzy is v1.x non-goal.

use std::collections::HashMap;

/// Tracks how often each (spec, prefix) tuple has been accepted via Tab.
/// Used to break ties when multiple suggestions match a prefix.
#[derive(Debug, Default)]
pub struct UsageCounter {
    counts: HashMap<(String, String), u64>,
}

impl UsageCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, spec: &str, accepted: &str) {
        let key = (spec.to_string(), accepted.to_string());
        *self.counts.entry(key).or_insert(0) += 1;
    }

    pub fn count(&self, spec: &str, accepted: &str) -> u64 {
        let key = (spec.to_string(), accepted.to_string());
        self.counts.get(&key).copied().unwrap_or(0)
    }
}

/// Score a candidate against a prefix. Higher = better. Negative means
/// no match.
///
/// **v1.0 contract** — strict prefix only:
/// - `score("commit", "co", _) ≥ 0`     (real prefix match)
/// - `score("checkout", "co", _) < 0`   (NOT a match — "checkout" starts with "ch", not "co")
/// - `score("checkout", "chk", _) < 0`  (no fuzzy)
pub fn score(candidate: &str, prefix: &str, usage_count: u64) -> i64 {
    if !candidate.starts_with(prefix) {
        return -1;
    }
    // Shorter remaining = closer match. Combine with usage count.
    let remaining = (candidate.len() - prefix.len()) as i64;
    let usage = usage_count.min(1_000) as i64; // cap to prevent overflow / runaway
    1_000_000 - remaining + usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matches_real_prefixes() {
        // "co" is a real prefix of "commit" and "config"
        assert!(score("commit", "co", 0) >= 0);
        assert!(score("config", "co", 0) >= 0);
        // "ch" is a real prefix of "checkout" and "cherry-pick"
        assert!(score("checkout", "ch", 0) >= 0);
        assert!(score("cherry-pick", "ch", 0) >= 0);
    }

    #[test]
    fn non_prefix_does_not_match() {
        // "checkout" does NOT start with "co" (starts with "ch") — common
        // misconception worth pinning in a test.
        assert!(score("checkout", "co", 0) < 0);
    }

    #[test]
    fn fuzzy_does_not_match_in_v1_0() {
        // No subsequence / fuzzy matching: "chk" is not a prefix of "checkout".
        assert!(score("checkout", "chk", 0) < 0);
        assert!(score("commit", "cmt", 0) < 0);
    }

    #[test]
    fn empty_prefix_matches_anything() {
        // Empty prefix == "show every subcommand" (the `git ⎵` case).
        assert!(score("commit", "", 0) >= 0);
        assert!(score("checkout", "", 0) >= 0);
    }

    #[test]
    fn usage_breaks_ties() {
        let cold = score("commit", "co", 0);
        let warm = score("commit", "co", 5);
        assert!(warm > cold);
    }

    #[test]
    fn shorter_remainder_wins_at_same_usage() {
        // "ch" → "checkout" (6 chars left) vs "ch" → "cherry-pick" (9 chars left):
        // shorter remainder wins when usage is equal.
        assert!(score("checkout", "ch", 0) > score("cherry-pick", "ch", 0));
    }
}
