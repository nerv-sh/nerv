//! misses — local tally of commands that produced an empty popup.
//!
//! When completion returns `no spec for <name>`, the daemon records the
//! name here. `nerv doctor` reads the tally back and shows the commands
//! the user types most often but has no spec for, which is the signal
//! for writing an overlay spec (`~/.config/nerv/specs/`).
//!
//! Persisted to `~/Library/Caches/nerv/misses.tsv`, one row per command:
//!
//! ```text
//! <name>\t<count>\t<last_unix>
//! ```
//!
//! Strictly local: the file lives in the cache dir, is never
//! transmitted anywhere, and `nerv uninstall` removes it with the rest
//! of the cache (`docs/uninstall-spec.md` §2). This is diagnostics, not
//! telemetry (PLAN §4 비목표).
//!
//! Write-side mirrors [`crate::frecency::FrecencyStore`]: an in-memory
//! table plus [`MissCounter::flush_if_dirty`], which callers invoke
//! freely. Two policies make that safe — writes are throttled to one
//! per [`MIN_FLUSH_INTERVAL`] (a miss happens on every keystroke of a
//! spec-less command) and go through [`crate::paths::write_atomic`],
//! so `nerv doctor` reading concurrently never sees a half-written
//! tally. Daemon shutdown calls [`MissCounter::flush_now`] to persist
//! the last window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::frecency::now_unix;

/// Upper bound on tracked command names. The tally only ever feeds a
/// top-5 doctor row, so an unbounded file would be pure waste — a
/// typo'd command name is recorded exactly like a real one. On
/// overflow the least-recorded entry is dropped (ties broken by the
/// older timestamp), which keeps the commands the user actually types.
const MAX_ENTRIES: usize = 200;

/// Shortest gap between two on-disk writes. Every keystroke against a
/// spec-less command is a `record`, so an unthrottled flush would
/// rewrite the whole file once per keypress — inside the completion the
/// widget is waiting on. The tally is advisory, so trading the last few
/// seconds of counts on an abrupt kill for one write per window is the
/// right side of that deal.
const MIN_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct Entry {
    count: u32,
    last_unix: u64,
}

/// Everything a flush has to see consistently, under one lock: the
/// rows, whether they changed since the last write, and when that
/// write was. Three separate mutexes would let a `record` slip between
/// "read dirty" and "clear dirty" and be lost.
#[derive(Debug, Default)]
struct State {
    table: HashMap<String, Entry>,
    dirty: bool,
    /// Bumped by every `record`. A flush clears `dirty` only if nothing
    /// was recorded while the lock was released for the write —
    /// otherwise that record would sit unflushed until the next one.
    version: u64,
    /// When the last successful write finished. `None` until the first
    /// one, so the opening flush of a session is never delayed.
    last_flush: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct MissCounter {
    path: Option<PathBuf>,
    state: Mutex<State>,
}

impl MissCounter {
    /// In-memory counter with no backing file — the `NERV_MISSES_FILE=-`
    /// form, and the default for tests.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load (or initialise) a counter backed by `path`. A missing file
    /// is an empty tally; malformed rows are skipped individually so a
    /// corrupted file can never break completion.
    pub fn load(path: &std::path::Path) -> Self {
        let mut map: HashMap<String, Entry> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((name, entry)) = parse_row(line) {
                    map.insert(name, entry);
                }
            }
        }
        Self {
            path: Some(path.to_path_buf()),
            state: Mutex::new(State {
                table: map,
                ..Default::default()
            }),
        }
    }

    /// Record one empty-popup command. Names that can't be a real
    /// executable stem (too short, path-like, or carrying the TSV
    /// delimiters) are ignored rather than stored.
    pub fn record(&self, name: &str) {
        if !crate::paths::is_command_stem(name) {
            return;
        }
        let now = now_unix();
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        if !st.table.contains_key(name) && st.table.len() >= MAX_ENTRIES {
            evict_one(&mut st.table);
        }
        let e = st.table.entry(name.to_string()).or_insert(Entry {
            count: 0,
            last_unix: now,
        });
        e.count = e.count.saturating_add(1);
        e.last_unix = now;
        st.dirty = true;
        st.version = st.version.wrapping_add(1);
    }

    /// The `n` most-recorded commands, highest count first. Ties break
    /// on the more recent timestamp, then the name, so the row is
    /// stable across runs.
    pub fn top_n(&self, n: usize) -> Vec<(String, u32)> {
        let Ok(st) = self.state.lock() else {
            return Vec::new();
        };
        let mut rows: Vec<(&String, &Entry)> = st.table.iter().collect();
        rows.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then(b.1.last_unix.cmp(&a.1.last_unix))
                .then(a.0.cmp(b.0))
        });
        rows.into_iter()
            .take(n)
            .map(|(name, e)| (name.clone(), e.count))
            .collect()
    }

    /// Write the tally back to disk if it changed since the last flush
    /// *and* [`MIN_FLUSH_INTERVAL`] has passed since the last write.
    /// Safe to call on every request — that is the intended cadence.
    /// Best-effort: a write failure must never break completion.
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
        // Serialize the rows under the lock, then release it before the
        // write. `record` runs inside the completion the widget is
        // waiting on; it must never queue behind an fsync.
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
            for (name, entry) in st.table.iter() {
                out.push_str(name);
                out.push('\t');
                out.push_str(&entry.count.to_string());
                out.push('\t');
                out.push_str(&entry.last_unix.to_string());
                out.push('\n');
            }
            (out, st.version)
        };
        if crate::paths::write_atomic(path, &out).is_ok() {
            if let Ok(mut st) = self.state.lock() {
                if st.version == version {
                    st.dirty = false;
                }
                st.last_flush = Some(Instant::now());
            }
        }
    }

    /// Live entry count — diagnostics only.
    pub fn len(&self) -> usize {
        self.state.lock().map(|st| st.table.len()).unwrap_or(0)
    }

    /// Whether anything has been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Drop the least-recorded entry (oldest wins the tie).
fn evict_one(table: &mut HashMap<String, Entry>) {
    let victim = table
        .iter()
        .min_by(|a, b| {
            a.1.count
                .cmp(&b.1.count)
                .then(a.1.last_unix.cmp(&b.1.last_unix))
        })
        .map(|(k, _)| k.clone());
    if let Some(k) = victim {
        table.remove(&k);
    }
}

fn parse_row(line: &str) -> Option<(String, Entry)> {
    let mut parts = line.splitn(3, '\t');
    let name = parts.next()?.to_string();
    let count: u32 = parts.next()?.parse().ok()?;
    let last_unix: u64 = parts.next()?.parse().ok()?;
    if !crate::paths::is_command_stem(&name) {
        return None;
    }
    Some((name, Entry { count, last_unix }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "nerv-misses-{tag}-{}-{}.tsv",
            std::process::id(),
            now_unix()
        ));
        p
    }

    #[test]
    fn record_then_top_n_reports_the_command() {
        let c = MissCounter::empty();
        c.record("zeph");
        assert_eq!(c.top_n(5), vec![("zeph".to_string(), 1)]);
    }

    #[test]
    fn counts_accumulate_and_sort_by_count_desc() {
        let c = MissCounter::empty();
        for _ in 0..3 {
            c.record("aic2");
        }
        c.record("zeph");
        for _ in 0..7 {
            c.record("aicommit2");
        }
        assert_eq!(
            c.top_n(2),
            vec![("aicommit2".to_string(), 7), ("aic2".to_string(), 3)]
        );
    }

    #[test]
    fn top_n_truncates_to_n() {
        let c = MissCounter::empty();
        for name in ["a1", "b2", "c3", "d4", "e5", "f6"] {
            c.record(name);
        }
        assert_eq!(c.top_n(5).len(), 5);
    }

    #[test]
    fn path_like_and_short_names_are_ignored() {
        let c = MissCounter::empty();
        for bad in ["", "x", "./foo", "/usr/bin/ls", "a\tb", "a\nb", "-flag"] {
            c.record(bad);
        }
        assert!(c.is_empty(), "rejected names must not be stored");
    }

    #[test]
    fn dotted_and_plus_names_are_recordable() {
        let c = MissCounter::empty();
        c.record("aws.cli");
        c.record("g++");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn flush_then_load_round_trips() {
        let path = tmp_path("roundtrip");
        let c = MissCounter::load(&path);
        c.record("zeph");
        c.record("zeph");
        c.record("aic2");
        c.flush_if_dirty();

        let reloaded = MissCounter::load(&path);
        assert_eq!(
            reloaded.top_n(5),
            vec![("zeph".to_string(), 2), ("aic2".to_string(), 1)]
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A second `flush_if_dirty` inside the interval does not touch the
    /// file — the guard against one full rewrite per keystroke. The
    /// in-memory count keeps climbing, so nothing is lost, only deferred.
    #[test]
    fn flush_is_throttled_within_the_interval() {
        let path = tmp_path("throttle");
        let c = MissCounter::load(&path);
        c.record("zeph");
        c.flush_if_dirty();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "zeph\t1\t".to_string() + &now_unix().to_string() + "\n",
            "first flush writes immediately",
        );

        c.record("zeph");
        c.record("aic2");
        c.flush_if_dirty();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk.lines().count(), 1, "throttled: {on_disk:?}");
        assert_eq!(c.top_n(2).len(), 2, "counts still accrue in memory");

        // A forced flush is the shutdown path — it ignores the interval.
        c.flush_now();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk.lines().count(), 2, "{on_disk:?}");
        assert_eq!(
            MissCounter::load(&path).top_n(1),
            vec![("zeph".to_string(), 2)]
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The write replaces the file by rename, never by truncating it in
    /// place. `nerv doctor` reads this file from another process while
    /// the daemon may be writing, so a reader holding the old file must
    /// keep seeing a whole tally rather than a half-written one. A
    /// held descriptor keeps the old inode across a rename; under a
    /// truncate-in-place write it would observe the new bytes.
    #[test]
    fn flush_replaces_the_file_by_rename_not_truncation() {
        use std::io::Read;
        let path = tmp_path("atomic");
        let c = MissCounter::load(&path);
        c.record("zeph");
        c.flush_now();

        let mut held = std::fs::File::open(&path).expect("open pre-flush file");
        c.record("aic2");
        c.flush_now();

        let mut seen = String::new();
        held.read_to_string(&mut seen).expect("read held fd");
        assert_eq!(
            seen.lines().count(),
            1,
            "held descriptor must still see the pre-flush file: {seen:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            2,
            "the path itself must point at the new file"
        );
        let tmp = path.with_file_name(format!(
            "{}.nerv-tmp",
            path.file_name().unwrap().to_str().unwrap()
        ));
        assert!(!tmp.exists(), "temp file must be renamed away");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_counter_never_writes_a_file() {
        let path = tmp_path("nofile");
        let c = MissCounter::empty();
        c.record("zeph");
        c.flush_if_dirty();
        assert!(!path.exists());
    }

    #[test]
    fn flush_is_a_noop_when_not_dirty() {
        let path = tmp_path("clean");
        let c = MissCounter::load(&path);
        c.flush_if_dirty();
        assert!(!path.exists(), "a clean counter must not create the file");
    }

    #[test]
    fn corrupt_rows_are_skipped_not_fatal() {
        let path = tmp_path("corrupt");
        std::fs::write(
            &path,
            "zeph\t4\t1700000000\ngarbage\nbad\tcount\tx\n./nope\t9\t1700000000\naic2\t2\t1700000001\n",
        )
        .unwrap();
        let c = MissCounter::load(&path);
        assert_eq!(
            c.top_n(5),
            vec![("zeph".to_string(), 4), ("aic2".to_string(), 2)]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn entry_count_is_bounded_and_keeps_the_frequent_names() {
        let c = MissCounter::empty();
        // One heavily-recorded name plus MAX_ENTRIES of one-shot noise.
        for _ in 0..50 {
            c.record("zeph");
        }
        for i in 0..MAX_ENTRIES + 20 {
            c.record(&format!("noise{i}"));
        }
        assert!(c.len() <= MAX_ENTRIES, "len was {}", c.len());
        assert_eq!(c.top_n(1), vec![("zeph".to_string(), 50)]);
    }
}
