//! frecency — tiny per-spec usage tracker that boosts repeat picks.
//!
//! Persists to `~/Library/Caches/nerv/frecency.tsv` as one row per
//! accepted suggestion:
//!
//! ```text
//! <spec_name>\t<insertion>\t<count>\t<last_unix>
//! ```
//!
//! Read-side: a [`FrecencyStore`] holds the parsed map and answers
//! `score(spec, insertion)` queries in O(1). Write-side: callers
//! invoke [`FrecencyStore::record`] when the user accepts a
//! suggestion; the store batches changes and flushes opportunistically
//! ([`FrecencyStore::flush_if_dirty`]).
//!
//! Score model: `count / (1 + age_days)`. Simple monotone in usage
//! and decays with time so abandoned picks naturally fall out of the
//! top spots. Not a Mozilla-style frecency proper — but small and
//! good enough for a ranking nudge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct Entry {
    count: u32,
    last_unix: u64,
}

#[derive(Debug, Default)]
pub struct FrecencyStore {
    path: Option<PathBuf>,
    // Keyed on `(spec_name, insertion)`.
    table: Mutex<HashMap<(String, String), Entry>>,
    dirty: Mutex<bool>,
}

impl FrecencyStore {
    /// In-memory store with no backing file. Useful for tests.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load (or initialise) a store backed by `path`. A missing file
    /// is treated as an empty store. Parse errors silently skip the
    /// offending row so a corrupted history can't break completion.
    pub fn load(path: &std::path::Path) -> Self {
        let mut map: HashMap<(String, String), Entry> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some(entry) = parse_row(line) {
                    map.insert(entry.0, entry.1);
                }
            }
        }
        Self {
            path: Some(path.to_path_buf()),
            table: Mutex::new(map),
            dirty: Mutex::new(false),
        }
    }

    /// Record an accepted suggestion. Increments the count and stamps
    /// the current time. Marks the store dirty for a later flush.
    pub fn record(&self, spec: &str, insertion: &str) {
        let now = now_unix();
        if let Ok(mut table) = self.table.lock() {
            let key = (spec.to_string(), insertion.to_string());
            let e = table.entry(key).or_insert(Entry {
                count: 0,
                last_unix: now,
            });
            e.count = e.count.saturating_add(1);
            e.last_unix = now;
        }
        if let Ok(mut dirty) = self.dirty.lock() {
            *dirty = true;
        }
    }

    /// Return the score for one suggestion. `0.0` when not seen
    /// *or* seen exactly once — a single accidental pick should not
    /// be enough to override the alpha order. Multi-pick entries
    /// score `(count - 1) / (1 + age_days)`: monotone in count, with
    /// linear decay so a long-unused entry naturally drops back into
    /// the no-boost band.
    pub fn score(&self, spec: &str, insertion: &str) -> f64 {
        let table = match self.table.lock() {
            Ok(t) => t,
            Err(_) => return 0.0,
        };
        let key = (spec.to_string(), insertion.to_string());
        let Some(entry) = table.get(&key) else {
            return 0.0;
        };
        if entry.count < 2 {
            return 0.0;
        }
        let now = now_unix();
        let age_days = (now.saturating_sub(entry.last_unix)) as f64 / 86_400.0;
        ((entry.count - 1) as f64) / (1.0 + age_days)
    }

    /// Write the in-memory table back to disk if it's been mutated
    /// since the last flush. Best-effort: errors silently dropped
    /// (frecency is advisory; a write failure must never break
    /// completion).
    pub fn flush_if_dirty(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let dirty = match self.dirty.lock() {
            Ok(d) => *d,
            Err(_) => return,
        };
        if !dirty {
            return;
        }
        // Serialize under the lock, then release it before the write.
        // `score()` takes this same mutex once per suggestion while
        // ranking a completion; it must never queue behind an fsync.
        let out = {
            let table = match self.table.lock() {
                Ok(t) => t,
                Err(_) => return,
            };
            let mut out = String::new();
            for ((spec, ins), entry) in table.iter() {
                // Skip entries whose strings contain TAB or newline — the
                // row encoding can't represent them. Should be unreachable
                // for normal suggestions but defends the on-disk format.
                if spec.contains('\t') || spec.contains('\n') {
                    continue;
                }
                if ins.contains('\t') || ins.contains('\n') {
                    continue;
                }
                out.push_str(spec);
                out.push('\t');
                out.push_str(ins);
                out.push('\t');
                out.push_str(&entry.count.to_string());
                out.push('\t');
                out.push_str(&entry.last_unix.to_string());
                out.push('\n');
            }
            out
        };
        // temp+rename: the CLI reads this file while the daemon writes it.
        if crate::paths::write_atomic(path, &out).is_ok() {
            if let Ok(mut d) = self.dirty.lock() {
                *d = false;
            }
        }
    }

    /// Live entry count — for diagnostics / `nerv doctor`.
    pub fn len(&self) -> usize {
        self.table.lock().map(|t| t.len()).unwrap_or(0)
    }

    /// Whether the store has any tracked entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn parse_row(line: &str) -> Option<((String, String), Entry)> {
    let mut parts = line.splitn(4, '\t');
    let spec = parts.next()?.to_string();
    let ins = parts.next()?.to_string();
    let count: u32 = parts.next()?.parse().ok()?;
    let last_unix: u64 = parts.next()?.parse().ok()?;
    if spec.is_empty() || ins.is_empty() {
        return None;
    }
    Some(((spec, ins), Entry { count, last_unix }))
}

/// Seconds since the epoch — the timestamp both TSV tallies stamp.
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_returns_zero_score() {
        let s = FrecencyStore::empty();
        assert_eq!(s.score("git", "checkout"), 0.0);
    }

    #[test]
    fn single_pick_returns_zero() {
        // Threshold: single pick is treated as accidental and gets
        // no boost — only multi-picks earn a non-zero score.
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        assert_eq!(s.score("git", "checkout"), 0.0);
    }

    #[test]
    fn two_picks_gives_score_one() {
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        s.record("git", "checkout");
        let sc = s.score("git", "checkout");
        // (count - 1) / (1 + age=0) = 1.
        assert!(
            (0.99..=1.01).contains(&sc),
            "expected ~1.0 after 2 hits, got {sc}"
        );
    }

    #[test]
    fn more_picks_score_higher() {
        let s = FrecencyStore::empty();
        for _ in 0..5 {
            s.record("git", "checkout");
        }
        s.record("git", "status");
        s.record("git", "status");
        assert!(s.score("git", "checkout") > s.score("git", "status"));
    }

    #[test]
    fn score_isolates_by_spec_name() {
        let s = FrecencyStore::empty();
        s.record("git", "co");
        assert_eq!(s.score("svn", "co"), 0.0);
    }

    #[test]
    fn roundtrip_through_disk_preserves_score() {
        let tmp = std::env::temp_dir().join(format!(
            "nerv-frecency-{}-{}.tsv",
            std::process::id(),
            now_unix()
        ));
        let s1 = FrecencyStore::load(&tmp);
        s1.record("git", "checkout");
        s1.record("git", "checkout");
        s1.record("git", "checkout");
        s1.record("git", "status");
        s1.flush_if_dirty();

        let s2 = FrecencyStore::load(&tmp);
        // checkout (3 picks) > status (1 pick, no boost).
        assert!(s2.score("git", "checkout") > s2.score("git", "status"));
        assert_eq!(s2.len(), 2);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn parse_row_rejects_short_lines() {
        assert!(parse_row("only_two\tfields").is_none());
        assert!(parse_row("a\tb\tnan\t1").is_none());
        assert!(parse_row("a\tb\t3\tnotanumber").is_none());
    }
}
