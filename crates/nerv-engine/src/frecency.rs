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
//! ([`FrecencyStore::flush_if_dirty`], at most once per five seconds),
//! and keeps at most [`MAX_ENTRIES`] rows.
//!
//! Score model: `ln(1 + count) · exp(-age / 1 week)` — deja's frecency
//! ([`crate::history::frecency`]), shared with the history ranking.
//! A single accept counts; the log keeps a hundred picks from burying
//! everything else, and the decay lets an abandoned pick fall back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct Entry {
    count: u32,
    last_unix: u64,
}

/// Most rows the store keeps. Every accept of a new `(spec, insertion)`
/// pair adds one and nothing ever removed them, so the table — walked by
/// [`FrecencyStore::spec_names`] on each command-name keystroke and
/// rewritten whole on each flush — grew for as long as nerv was
/// installed. A measured store held 518 rows; four times that is past
/// any working set. On overflow the lowest-scored row goes.
pub const MAX_ENTRIES: usize = 2000;

/// Shortest gap between two on-disk writes — the miss tally's rule
/// (`misses::MIN_FLUSH_INTERVAL`). The counts are advisory, and the last
/// window is written by [`FrecencyStore::flush_now`] at shutdown.
const MIN_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Everything a flush has to see consistently, under one lock (see
/// `misses::State`).
#[derive(Debug, Default)]
struct State {
    /// spec name → insertion → entry. Nested so a lookup borrows both
    /// strings: ranking asks for every row of a popup, and a flat
    /// `(String, String)` key cost two allocations per row.
    table: HashMap<String, HashMap<String, Entry>>,
    /// Entries across all specs — what [`MAX_ENTRIES`] bounds.
    rows: usize,
    dirty: bool,
    /// Bumped by every `record`: a flush clears `dirty` only if nothing
    /// was recorded while the lock was released for the write.
    version: u64,
    /// `None` until the first write, so that one is never delayed.
    last_flush: Option<Instant>,
    /// [`FrecencyStore::spec_names`] as last computed, with the version
    /// and the minute it was computed in. The list is asked for on every
    /// command-name keystroke and changes only on an accept, or as the
    /// decay slowly reorders it.
    names: Option<(u64, u64, Vec<String>)>,
}

#[derive(Debug, Default)]
pub struct FrecencyStore {
    path: Option<PathBuf>,
    state: Mutex<State>,
}

impl FrecencyStore {
    /// In-memory store with no backing file. Useful for tests.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load (or initialise) a store backed by `path`. A missing file
    /// is treated as an empty store. Parse errors silently skip the
    /// offending row so a corrupted history can't break completion.
    /// A file from before the cap is cut down to its best rows.
    pub fn load(path: &std::path::Path) -> Self {
        let mut flat: HashMap<(String, String), Entry> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some(entry) = parse_row(line) {
                    flat.insert(entry.0, entry.1);
                }
            }
        }
        let dirty = flat.len() > MAX_ENTRIES;
        let mut rows: Vec<_> = flat.into_iter().collect();
        if dirty {
            let now = now_unix();
            rows.sort_by(|a, b| score_of(&b.1, now).total_cmp(&score_of(&a.1, now)));
            rows.truncate(MAX_ENTRIES);
        }
        let mut st = State {
            rows: rows.len(),
            dirty,
            ..Default::default()
        };
        for ((spec, insertion), entry) in rows {
            st.table.entry(spec).or_default().insert(insertion, entry);
        }
        Self {
            path: Some(path.to_path_buf()),
            state: Mutex::new(st),
        }
    }

    /// Record an accepted suggestion. Increments the count and stamps
    /// the current time. Marks the store dirty for a later flush.
    pub fn record(&self, spec: &str, insertion: &str) {
        let now = now_unix();
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        let known = st
            .table
            .get(spec)
            .is_some_and(|m| m.contains_key(insertion));
        if !known {
            if st.rows >= MAX_ENTRIES {
                st.evict_one(now);
            }
            st.rows += 1;
        }
        let e = st
            .table
            .entry(spec.to_string())
            .or_default()
            .entry(insertion.to_string())
            .or_insert(Entry {
                count: 0,
                last_unix: now,
            });
        e.count = e.count.saturating_add(1);
        e.last_unix = now;
        st.dirty = true;
        st.version = st.version.wrapping_add(1);
    }

    /// The score for one suggestion: [`crate::history::frecency`] of the
    /// pair, `ln(1 + count) · exp(-age / 1 week)`. Zero only when never
    /// accepted — a single pick counts.
    pub fn score(&self, spec: &str, insertion: &str) -> f64 {
        self.scores(spec, [insertion])[0]
    }

    /// [`Self::score`] of each insertion, in order, under one lock and
    /// one clock reading — what ranking a popup of hundreds of rows asks.
    pub fn scores<'a>(
        &self,
        spec: &str,
        insertions: impl IntoIterator<Item = &'a str>,
    ) -> Vec<f64> {
        let st = self.state.lock().ok();
        let picks = st.as_ref().and_then(|st| st.table.get(spec));
        let now = now_unix();
        insertions
            .into_iter()
            .map(|ins| {
                picks
                    .and_then(|m| m.get(ins))
                    .map_or(0.0, |entry| score_of(entry, now))
            })
            .collect()
    }

    /// Command names the user has accepted a suggestion for, most-used
    /// first. Keys are `(spec, insertion)` pairs, so a name's weight is
    /// the sum over its rows of [`Self::score`]'s frecency. Ties break
    /// alphabetically so the list is stable across calls.
    pub fn spec_names(&self) -> Vec<String> {
        let Ok(mut st) = self.state.lock() else {
            return vec![];
        };
        let now = now_unix();
        let stamp = (st.version, now / 60);
        if let Some((version, minute, names)) = &st.names {
            if (*version, *minute) == stamp {
                return names.clone();
            }
        }
        let mut names: Vec<(&str, f64)> = st
            .table
            .iter()
            .map(|(spec, picks)| {
                let weight = picks.values().map(|e| score_of(e, now)).sum();
                (spec.as_str(), weight)
            })
            .collect();
        names.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let names: Vec<String> = names.into_iter().map(|(n, _)| n.to_string()).collect();
        st.names = Some((stamp.0, stamp.1, names.clone()));
        names
    }

    /// Write the in-memory table back to disk if it changed since the
    /// last flush *and* [`MIN_FLUSH_INTERVAL`] has passed since the last
    /// write. Best-effort: errors silently dropped (frecency is advisory;
    /// a write failure must never break completion).
    pub fn flush_if_dirty(&self) {
        self.flush_inner(false);
    }

    /// Flush regardless of the interval, for a caller that knows this is
    /// the last chance (daemon shutdown, end of a test).
    pub fn flush_now(&self) {
        self.flush_inner(true);
    }

    fn flush_inner(&self, force: bool) {
        let Some(path) = &self.path else {
            return;
        };
        // Serialize under the lock, then release it before the write.
        // `score()` takes this same mutex once per suggestion while
        // ranking a completion; it must never queue behind an fsync.
        let (out, version) = {
            let Ok(st) = self.state.lock() else {
                return;
            };
            if !st.dirty {
                return;
            }
            if !force
                && st
                    .last_flush
                    .is_some_and(|t| t.elapsed() < MIN_FLUSH_INTERVAL)
            {
                return;
            }
            let mut out = String::new();
            let rows = st
                .table
                .iter()
                .flat_map(|(spec, picks)| picks.iter().map(move |(ins, e)| (spec, ins, e)));
            for (spec, ins, entry) in rows {
                // Skip entries whose strings contain TAB or newline — the
                // row encoding can't represent them. Should be unreachable
                // for normal suggestions but defends the on-disk format.
                if spec.contains(['\t', '\n']) || ins.contains(['\t', '\n']) {
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
            (out, st.version)
        };
        // temp+rename: the CLI reads this file while the daemon writes it.
        if crate::paths::write_atomic(path, &out).is_ok() {
            if let Ok(mut st) = self.state.lock() {
                if st.version == version {
                    st.dirty = false;
                }
                st.last_flush = Some(Instant::now());
            }
        }
    }

    /// Live entry count — for diagnostics / `nerv doctor`.
    pub fn len(&self) -> usize {
        self.state.lock().map(|st| st.rows).unwrap_or(0)
    }

    /// Whether the store has any tracked entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn score_of(entry: &Entry, now: u64) -> f64 {
    crate::history::frecency(entry.count, entry.last_unix, now)
}

impl State {
    /// Drop the lowest-scored row (the older one on a tie).
    fn evict_one(&mut self, now: u64) {
        let victim = self
            .table
            .iter()
            .flat_map(|(spec, picks)| picks.iter().map(move |(ins, e)| (spec, ins, e)))
            .min_by(|a, b| {
                score_of(a.2, now)
                    .total_cmp(&score_of(b.2, now))
                    .then(a.2.last_unix.cmp(&b.2.last_unix))
            })
            .map(|(spec, ins, _)| (spec.clone(), ins.clone()));
        let Some((spec, ins)) = victim else {
            return;
        };
        if let Some(picks) = self.table.get_mut(&spec) {
            picks.remove(&ins);
            if picks.is_empty() {
                self.table.remove(&spec);
            }
            self.rows -= 1;
        }
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
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_names_is_empty_without_history() {
        assert!(FrecencyStore::empty().spec_names().is_empty());
    }

    /// Command-name ranking sums a name's rows: `git` (two accepts on
    /// one insertion + one on another) outweighs `brew` (one).
    #[test]
    fn spec_names_ranks_by_total_accepts() {
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        s.record("git", "checkout");
        s.record("git", "status");
        s.record("brew", "install");
        assert_eq!(s.spec_names(), vec!["git", "brew"]);
    }

    /// Equal weight must not leave the order to HashMap iteration —
    /// the popup would reshuffle between keystrokes.
    #[test]
    fn spec_names_breaks_ties_alphabetically() {
        let s = FrecencyStore::empty();
        for spec in ["npm", "cargo", "docker"] {
            s.record(spec, "x");
        }
        // Pin the timestamps: a second boundary landing mid-loop would
        // decay the earlier rows and make this a recency test instead.
        if let Ok(mut st) = s.state.lock() {
            let now = now_unix();
            for entry in st.table.values_mut().flat_map(HashMap::values_mut) {
                entry.last_unix = now;
            }
        }
        assert_eq!(s.spec_names(), vec!["cargo", "docker", "npm"]);
    }

    /// A name last used long ago decays below a fresher one with the
    /// same count — same recency model as `score`.
    #[test]
    fn spec_names_decays_stale_entries() {
        let s = FrecencyStore::empty();
        s.record("stale", "x");
        s.record("fresh", "x");
        if let Ok(mut st) = s.state.lock() {
            let e = st
                .table
                .get_mut("stale")
                .and_then(|m| m.get_mut("x"))
                .expect("recorded above");
            // 9 days back → weight e^(-9/7) ≈ 0.28 of the fresh entry's.
            e.last_unix = now_unix() - 9 * 86_400;
        }
        assert_eq!(s.spec_names(), vec!["fresh", "stale"]);
    }

    #[test]
    fn empty_store_returns_zero_score() {
        let s = FrecencyStore::empty();
        assert_eq!(s.score("git", "checkout"), 0.0);
    }

    #[test]
    fn single_pick_counts() {
        // One accept is a signal too (the old model deadbanded it to 0).
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        let sc = s.score("git", "checkout");
        assert!((sc - 2f64.ln()).abs() < 0.01, "ln 2 after 1 hit, got {sc}");
    }

    #[test]
    fn two_picks_score_ln_three() {
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        s.record("git", "checkout");
        let sc = s.score("git", "checkout");
        assert!((sc - 3f64.ln()).abs() < 0.01, "ln 3 after 2 hits, got {sc}");
    }

    #[test]
    fn a_week_old_pick_decays_by_e() {
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        if let Ok(mut st) = s.state.lock() {
            for e in st.table.values_mut().flat_map(HashMap::values_mut) {
                e.last_unix = now_unix() - 7 * 86_400;
            }
        }
        let sc = s.score("git", "checkout");
        assert!((sc - 2f64.ln() / std::f64::consts::E).abs() < 0.01, "{sc}");
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

    /// The table is walked on every command-name keystroke and written
    /// whole on every flush, so it has a ceiling: past it the weakest
    /// row makes room, and a pick the user repeats is never that row.
    #[test]
    fn the_store_is_capped_and_keeps_what_is_used() {
        let s = FrecencyStore::empty();
        for _ in 0..3 {
            s.record("git", "checkout");
        }
        for i in 0..MAX_ENTRIES + 50 {
            s.record("tool", &format!("once-{i}"));
        }
        assert_eq!(s.len(), MAX_ENTRIES);
        assert!(s.score("git", "checkout") > 0.0);
        assert_eq!(s.spec_names()[0], "tool");
    }

    /// A file written before the cap existed is cut to its best rows on
    /// load, and the cut reaches the disk.
    #[test]
    fn an_oversized_file_is_trimmed_on_load() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("frecency.tsv");
        let now = now_unix();
        let mut text = format!("git\tcheckout\t9\t{now}\n");
        for i in 0..MAX_ENTRIES + 10 {
            text.push_str(&format!("tool\tonce-{i}\t1\t{}\n", now - 86_400));
        }
        std::fs::write(&path, text).unwrap();
        let s = FrecencyStore::load(&path);
        assert_eq!(s.len(), MAX_ENTRIES);
        assert!(s.score("git", "checkout") > 0.0);
        s.flush_if_dirty();
        assert_eq!(FrecencyStore::load(&path).len(), MAX_ENTRIES);
    }

    /// An accept is not a disk write: within the interval the second
    /// one waits, and the shutdown flush still lands it.
    #[test]
    fn flushes_are_throttled_and_the_last_window_is_kept() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("frecency.tsv");
        let s = FrecencyStore::load(&path);
        s.record("git", "checkout");
        s.flush_if_dirty();
        s.record("git", "status");
        s.flush_if_dirty();
        assert_eq!(FrecencyStore::load(&path).len(), 1);
        s.flush_now();
        assert_eq!(FrecencyStore::load(&path).len(), 2);
    }

    /// One call scores a whole popup, in the order asked.
    #[test]
    fn scores_answers_every_insertion_in_order() {
        let s = FrecencyStore::empty();
        s.record("git", "checkout");
        s.record("git", "checkout");
        s.record("git", "status");
        s.record("svn", "commit");
        let got = s.scores("git", ["status", "commit", "checkout"]);
        assert_eq!(got.len(), 3);
        assert!(got[2] > got[0] && got[0] > 0.0);
        assert_eq!(got[1], 0.0, "another spec's pick does not count");
        assert_eq!(s.scores("nope", ["x"]), [0.0]);
    }

    /// The name list is kept between keystrokes and dropped by a record.
    #[test]
    fn spec_names_follow_a_new_record() {
        let s = FrecencyStore::empty();
        s.record("git", "status");
        assert_eq!(s.spec_names(), ["git"]);
        s.record("npm", "run");
        s.record("npm", "run");
        assert_eq!(s.spec_names(), ["npm", "git"]);
    }

    #[test]
    fn parse_row_rejects_short_lines() {
        assert!(parse_row("only_two\tfields").is_none());
        assert!(parse_row("a\tb\tnan\t1").is_none());
        assert!(parse_row("a\tb\t3\tnotanumber").is_none());
    }
}
