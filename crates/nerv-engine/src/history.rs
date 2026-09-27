//! Executed-command history: the record behind history-ranked ghosts and
//! the popup's directory / sequence signals.
//!
//! Persisted as `~/Library/Caches/nerv/history.tsv`, one command per line:
//!
//! ```text
//! <unix ts>\t<exit>\t<cwd>\t<prev>\t<command>\t<expanded>
//! ```
//!
//! `command` is the line as typed (what a ghost offers back); `expanded`
//! is zsh's alias-expanded form, empty when identical. Fields escape `\`,
//! tab and newline, so a multi-line command stays one row.
//!
//! The file is append-only: one command costs one `O_APPEND` line. It is
//! rewritten whole only when it passes [`ROW_CAP`], keeping the newest
//! [`ROW_KEEP`] rows. It holds commands the user typed, secrets included
//! when they typed them, so it is created `0600` — the same trust as
//! `~/.zsh_history`. What zsh itself refuses to keep never gets here: the
//! widget filters `hist_ignore_space` and `HISTORY_IGNORE` before sending.
//! [`HistoryStore::record`] can only re-check the first of those — it
//! drops any leading-space command, whatever the shell option says — and
//! never sees `HISTORY_IGNORE`, whose pattern lives in the shell.
//!
//! Every write takes `history.tsv.lock` (flock), so the daemon, a CLI
//! appending while no daemon runs, and an import or compaction that
//! replaces the file by rename never lose each other's rows.
//!
//! Aggregates are rebuilt at load and kept current on every record, so
//! a keystroke never reads the file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use crate::frecency::now_unix;

/// Rows past which the file is compacted. At ~60 bytes a row this is a
/// few MB on disk.
pub const ROW_CAP: usize = 100_000;
/// Rows a compaction keeps (the newest).
pub const ROW_KEEP: usize = 80_000;
/// How much of a zsh history file an import reads: its tail, the newest
/// commands. 80,000 rows fit many times over; the bound keeps a request
/// naming a huge file (or `/dev/zero`) from exhausting the daemon.
const IMPORT_MAX_BYTES: u64 = 64 << 20;

/// One executed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub ts: u64,
    pub exit: i32,
    pub cwd: String,
    pub prev: String,
    pub command: String,
    /// Alias-expanded form; empty when it equals `command`.
    pub expanded: String,
}

/// Per-command totals.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stat {
    pub count: u32,
    pub last: u64,
    /// Runs with a known directory (imported rows have none) — the
    /// denominator of directory affinity.
    pub in_dirs: u32,
    /// [`Stat::frecency_key`], kept current on every add.
    key: f64,
    /// Multi-line or tab-bearing: never a ghost.
    no_ghost: bool,
}

impl Stat {
    /// `ln(frecency)` plus `now / 1 week` — a time-independent key.
    /// frecency = ln(1+count)·exp(-(now-last)/week), so
    /// ln(frecency) = ln(ln(1+count)) + last/week − now/week, and the
    /// ratio of two frecencies at any instant is `exp(key_a − key_b)`.
    /// Ranking needs no `now`, and no `exp` except for the few candidates
    /// a boost could lift.
    fn frecency_key(&self) -> f64 {
        (self.count as f64).ln_1p().ln() + self.last as f64 / DECAY_SECS
    }
}

/// FxHash (the rustc-hash 1.x algorithm) for the small integer keys of
/// the aggregates:
/// a keystroke looks up thousands of them, and SipHash made the 100k-row
/// worst case miss the keystroke budget (27.6 ms → see `history_100k_rss`).
/// HashDoS resistance buys nothing for keys the daemon mints itself.
#[derive(Default, Clone, Copy)]
struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;

/// zsh's "command not found". A typo (`gti status`) is recorded like any
/// row but never counted, so it is never offered back.
const EXIT_NOT_FOUND: i32 = 127;

/// Ranking weights, from deja's scorer (`internal/scorer/scorer.go`):
/// sequence 0.5, frecency 0.4, directory 0.3. Its fuzzy term is constant
/// here — a ghost only ever extends what was typed — so it drops out.
/// The command-head sequence term is nerv's: `git add .` and
/// `git add src/x.rs` are different predecessors to deja.
pub const W_SEQ: f64 = 0.5;
pub const W_SEQ_HEAD: f64 = 0.25;
pub const W_FRECENCY: f64 = 0.4;
pub const W_DIR: f64 = 0.3;
/// Times a command must have followed another before an empty prompt
/// predicts it.
pub const PREDICT_MIN: u32 = 2;
/// Recency decay constant of frecency: `exp(-age / 1 week)`.
pub const DECAY_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// `ln(1 + count) · exp(-age / 1 week)`, the frecency deja uses. The
/// reference form of [`Stat::frecency_key`], which ranking uses instead.
pub fn frecency(count: u32, last: u64, now: u64) -> f64 {
    let age = now.saturating_sub(last) as f64;
    (count as f64).ln_1p() * (-age / DECAY_SECS).exp()
}

/// The word a popup row and a history word are compared by: trailing
/// whitespace and `/` go (`src/` = `src`), and an option keeps only its
/// name (`--mode=` and `--mode=fast` are both `--mode`).
pub fn token_key(s: &str) -> &str {
    let s = s.trim_end();
    let s = if s.starts_with('-') {
        s.split('=').next().unwrap_or(s)
    } else {
        s
    };
    let t = s.trim_end_matches('/');
    if t.is_empty() { s } else { t }
}

/// What the history says about each word that followed a given line —
/// the popup's directory and "you type this" signals. Keyed by
/// [`token_key`].
#[derive(Debug, Default)]
pub struct TokenSignals {
    pub tokens: HashMap<String, TokenStat>,
}

#[derive(Debug, Default, Clone)]
pub struct TokenStat {
    /// Runs of commands that had this word here.
    pub count: u32,
    pub last: u64,
    /// Of those runs, how many were in the current directory …
    pub here: u32,
    /// … out of how many with any recorded directory.
    pub in_dirs: u32,
    /// Runs that directly followed the previous command, and its head.
    pub after_prev: u32,
    pub after_head: u32,
    /// The word as it was typed (the key is normalised), for a row the
    /// spec does not have.
    pub word: String,
}

impl TokenSignals {
    pub fn get(&self, insertion: &str) -> Option<&TokenStat> {
        self.tokens.get(token_key(insertion))
    }
}

/// The first two words of a command — `git add` of `git add src/x.rs`.
/// `None` when that is the whole command: the exact-prev term covers it.
fn head(cmd: &str) -> Option<String> {
    let mut words = cmd.split_whitespace();
    let h = format!("{} {}", words.next()?, words.next()?);
    words.next().is_some().then_some(h)
}

/// Aggregates over the rows, with every distinct string (command, cwd)
/// interned once as a `u32` id. A 100k-row history is mostly distinct
/// commands; keyed by `String`, each one was cloned into four maps and
/// loading one grew RSS by 109 MiB; interned it is ~27 MiB (`history_100k_rss`).
#[derive(Debug, Default)]
struct Aggregates {
    rows: usize,
    /// Bumped by every change, so a cached query can tell it is stale.
    generation: u64,
    strings: Vec<Arc<str>>,
    /// Ordered, so the commands extending a prefix are one range scan.
    ids: BTreeMap<Arc<str>, u32>,
    /// Indexed by id; `count == 0` for a string that is only ever a
    /// cwd or a prev head.
    stats: Vec<Stat>,
    /// (command, cwd) → runs there.
    dirs: FxMap<(u32, u32), u32>,
    /// (prev, next) → times `next` directly followed `prev`.
    seq: FxMap<(u32, u32), u32>,
    /// prev → its most frequent follower's count (normaliser).
    seq_max: FxMap<u32, u32>,
    /// (head of prev, next) → times; see [`head`].
    seq_head: FxMap<(u32, u32), u32>,
    seq_head_max: FxMap<u32, u32>,
    /// (words, command id): each command as the shell ran it — alias
    /// expanded, whitespace collapsed — so the popup can ask "what came
    /// after `git` here?" with one range scan. See [`token_signals`].
    ///
    /// [`token_signals`]: HistoryStore::token_signals
    words: BTreeSet<(Arc<str>, u32)>,
}

impl Aggregates {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = self.strings.len() as u32;
        let s: Arc<str> = Arc::from(s);
        self.strings.push(s.clone());
        self.stats.push(Stat::default());
        self.ids.insert(s, id);
        id
    }

    fn id(&self, s: &str) -> Option<u32> {
        self.ids.get(s).copied()
    }

    fn add(&mut self, e: &Entry) {
        self.rows += 1;
        self.generation += 1;
        if e.exit == EXIT_NOT_FOUND {
            return;
        }
        let cmd = self.intern(&e.command);
        if self.stats[cmd as usize].count == 0 {
            let src = if e.expanded.is_empty() {
                &e.command
            } else {
                &e.expanded
            };
            let norm = src.split_whitespace().collect::<Vec<_>>().join(" ");
            let w = self.intern(&norm);
            self.words.insert((self.strings[w as usize].clone(), cmd));
        }
        let stat = &mut self.stats[cmd as usize];
        stat.count = stat.count.saturating_add(1);
        stat.last = stat.last.max(e.ts);
        stat.key = stat.frecency_key();
        stat.no_ghost = e.command.contains(['\n', '\t']);
        if !e.cwd.is_empty() {
            stat.in_dirs = stat.in_dirs.saturating_add(1);
            let cwd = self.intern(&e.cwd);
            *self.dirs.entry((cmd, cwd)).or_default() += 1;
        }
        if !e.prev.is_empty() {
            let prev = self.intern(&e.prev);
            let n = self.seq.entry((prev, cmd)).or_default();
            *n += 1;
            let n = *n;
            let max = self.seq_max.entry(prev).or_default();
            *max = (*max).max(n);
            if let Some(h) = head(&e.prev) {
                let h = self.intern(&h);
                let n = self.seq_head.entry((h, cmd)).or_default();
                *n += 1;
                let n = *n;
                let max = self.seq_head_max.entry(h).or_default();
                *max = (*max).max(n);
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct HistoryStore {
    path: Option<PathBuf>,
    agg: Mutex<Aggregates>,
    /// Serialises this process's writes; [`FileLock`] serialises them
    /// with other processes'.
    io: Mutex<()>,
    /// The last [`HistoryStore::token_signals`] answer. The words before
    /// the cursor stay the same for every keystroke of a word (`git c`,
    /// `git ch`, … all ask about `git`), so only the first pays the scan.
    signals_cache: Mutex<Option<SignalsCacheEntry>>,
}

#[derive(Debug)]
struct SignalsCacheEntry {
    prefix: String,
    cwd: String,
    prev: String,
    generation: u64,
    signals: Arc<TokenSignals>,
}

/// zsh's own rule for `hist_ignore_space`: the first character is a
/// space or a tab. Blank lines are never worth keeping either.
pub fn is_ignored(command: &str) -> bool {
    command.trim().is_empty() || command.starts_with([' ', '\t'])
}

impl HistoryStore {
    /// A store with no file behind it (tests, `NERV_HISTORY_FILE=-`).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load `path`, tolerating a missing or partly corrupt file: a row
    /// that does not parse is skipped, never fatal.
    pub fn load(path: &Path) -> Self {
        use std::io::BufRead;
        let mut agg = Aggregates::default();
        // Streamed: holding the whole file as one string would add its
        // size (several MB at the cap) to the daemon's peak for nothing.
        if let Ok(f) = std::fs::File::open(path) {
            for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
                if let Some(e) = parse_row(&line) {
                    agg.add(&e);
                }
            }
        }
        Self {
            path: Some(path.to_path_buf()),
            agg: Mutex::new(agg),
            io: Mutex::new(()),
            signals_cache: Mutex::new(None),
        }
    }

    /// Rows currently counted (after the last compaction).
    pub fn len(&self) -> usize {
        self.agg.lock().map(|a| a.rows).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Totals for one command, if it was ever run.
    pub fn stat(&self, command: &str) -> Option<Stat> {
        let a = self.agg.lock().ok()?;
        let st = a.stats[a.id(command)? as usize];
        (st.count > 0).then_some(st)
    }

    /// Runs of `command` recorded in `cwd`.
    pub fn runs_in(&self, command: &str, cwd: &str) -> u32 {
        self.agg
            .lock()
            .ok()
            .and_then(|a| a.dirs.get(&(a.id(command)?, a.id(cwd)?)).copied())
            .unwrap_or(0)
    }

    /// Times `next` directly followed `prev`.
    pub fn followed(&self, prev: &str, next: &str) -> u32 {
        self.agg
            .lock()
            .ok()
            .and_then(|a| a.seq.get(&(a.id(prev)?, a.id(next)?)).copied())
            .unwrap_or(0)
    }

    /// The history command to offer as the ghost for `typed`: the best
    /// scored command that strictly extends it. `None` when nothing does.
    /// Single-line commands without a tab only — a ghost is painted on one
    /// row, and the reply format turns a tab into a space.
    ///
    /// score = [`W_FRECENCY`]·frecency (normalised over the candidates)
    ///       + [`W_DIR`]·(runs in `cwd` / runs anywhere)
    ///       + [`W_SEQ`]·(times it followed `prev` / `prev`'s top follower)
    ///       + [`W_SEQ_HEAD`]·(the same for `prev`'s first two words)
    ///
    /// Ties go to the most recent run. One pass finds the best candidate
    /// on frecency alone and collects the few a directory or sequence
    /// boost could lift above it; only those are fully scored.
    pub fn ghost(&self, typed: &str, cwd: &str, prev: &str) -> Option<String> {
        if typed.is_empty() {
            return None;
        }
        let a = self.agg.lock().ok()?;
        let cwd = a.id(cwd);
        let prev_id = a.id(prev);
        let head_id = head(prev).and_then(|h| a.id(&h));
        let ratio = |n: u32, d: u32| if d == 0 { 0.0 } else { n as f64 / d as f64 };
        let seq_term =
            |map: &FxMap<(u32, u32), u32>, max: &FxMap<u32, u32>, key: Option<u32>, id: u32| {
                key.map_or(0.0, |k| {
                    ratio(
                        map.get(&(k, id)).copied().unwrap_or(0),
                        max.get(&k).copied().unwrap_or(0),
                    )
                })
            };
        // (key, last, id) of the best on frecency; boosted (boost, key, last, id).
        let mut top: Option<(f64, u64, u32)> = None;
        let mut boosted: Vec<(f64, f64, u64, u32)> = Vec::new();
        let range = a
            .ids
            .range::<str, _>((std::ops::Bound::Included(typed), std::ops::Bound::Unbounded));
        for (s, &id) in range {
            if !s.starts_with(typed) {
                break;
            }
            let st = a.stats[id as usize];
            if st.count == 0 || st.no_ghost || s.len() == typed.len() {
                continue;
            }
            let key = st.key;
            if top.is_none_or(|(k, l, _)| (key, st.last) > (k, l)) {
                top = Some((key, st.last, id));
            }
            let d = cwd.map_or(0.0, |c| {
                ratio(a.dirs.get(&(id, c)).copied().unwrap_or(0), st.in_dirs)
            });
            let boost = W_DIR * d
                + W_SEQ * seq_term(&a.seq, &a.seq_max, prev_id, id)
                + W_SEQ_HEAD * seq_term(&a.seq_head, &a.seq_head_max, head_id, id);
            if boost > 0.0 {
                boosted.push((boost, key, st.last, id));
            }
        }
        let (kmax, top_last, top_id) = top?;
        let top_boost = boosted.iter().find(|b| b.3 == top_id).map_or(0.0, |b| b.0);
        boosted
            .iter()
            .map(|&(boost, key, last, id)| (W_FRECENCY * (key - kmax).exp() + boost, last, id))
            .chain(std::iter::once((W_FRECENCY + top_boost, top_last, top_id)))
            .max_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)))
            .map(|(_, _, id)| a.strings[id as usize].to_string())
    }

    /// For the words already typed on the line (`["git"]` while completing
    /// `git ch`), every word that came next in a recorded command, with its
    /// runs, recency and directory counts. Commands are matched on their
    /// alias-expanded form, so `g checkout` counts toward `git`'s rows.
    /// Empty `words` (the command word itself) returns nothing: command
    /// names have their own ranking.
    pub fn token_signals(&self, words: &[&str], cwd: &str, prev: &str) -> Arc<TokenSignals> {
        let mut out = TokenSignals::default();
        if words.is_empty() {
            return Arc::new(out);
        }
        let Ok(a) = self.agg.lock() else {
            return Arc::new(out);
        };
        let prefix = format!("{} ", words.join(" "));
        let mut cache = self.signals_cache.lock().ok();
        if let Some(Some(c)) = cache.as_deref() {
            if c.generation == a.generation && c.prefix == prefix && c.cwd == cwd && c.prev == prev
            {
                return c.signals.clone();
            }
        }
        let cwd_text = cwd;
        let cwd = a.id(cwd);
        let prev_id = a.id(prev);
        let head_id = head(prev).and_then(|h| a.id(&h));
        let seq = |map: &FxMap<(u32, u32), u32>, key: Option<u32>, cmd: u32| {
            key.map_or(0, |k| map.get(&(k, cmd)).copied().unwrap_or(0))
        };
        let start: Arc<str> = Arc::from(prefix.as_str());
        for (norm, cmd) in a.words.range((start, 0)..) {
            let Some(rest) = norm.strip_prefix(prefix.as_str()) else {
                break;
            };
            let Some(next) = rest.split(' ').next().filter(|w| !w.is_empty()) else {
                continue;
            };
            let st = a.stats[*cmd as usize];
            let here = cwd.map_or(0, |c| a.dirs.get(&(*cmd, c)).copied().unwrap_or(0));
            // Most entries under a prefix share a few next words; look the
            // key up before allocating one.
            let key = token_key(next);
            if !out.tokens.contains_key(key) {
                out.tokens.insert(
                    key.to_string(),
                    TokenStat {
                        word: next.to_string(),
                        ..TokenStat::default()
                    },
                );
            }
            let Some(t) = out.tokens.get_mut(key) else {
                continue;
            };
            t.count = t.count.saturating_add(st.count);
            t.last = t.last.max(st.last);
            t.here = t.here.saturating_add(here);
            t.in_dirs = t.in_dirs.saturating_add(st.in_dirs);
            t.after_prev = t.after_prev.saturating_add(seq(&a.seq, prev_id, *cmd));
            t.after_head = t.after_head.saturating_add(seq(&a.seq_head, head_id, *cmd));
        }
        let out = Arc::new(out);
        if let Some(cache) = cache.as_deref_mut() {
            *cache = Some(SignalsCacheEntry {
                prefix,
                cwd: cwd_text.to_string(),
                prev: prev.to_string(),
                generation: a.generation,
                signals: out.clone(),
            });
        }
        out
    }

    /// The command to offer on an empty prompt: what most often came
    /// right after `prev`. Only a habit seen at least [`PREDICT_MIN`] times
    /// counts — frecency alone would put a ghost on every prompt. `prev`'s
    /// command head (`git add` of `git add .`) is the fallback key. Ties go
    /// to the directory affinity, then the most recent run.
    pub fn predict(&self, prev: &str, cwd: &str) -> Option<String> {
        let a = self.agg.lock().ok()?;
        let cwd = a.id(cwd);
        let best = |map: &FxMap<(u32, u32), u32>, key: u32| {
            map.iter()
                .filter(|&(&(k, id), &n)| {
                    k == key && n >= PREDICT_MIN && !a.stats[id as usize].no_ghost
                })
                .map(|(&(_, id), &n)| {
                    let st = a.stats[id as usize];
                    let here = cwd.map_or(0, |c| a.dirs.get(&(id, c)).copied().unwrap_or(0));
                    (n, here, st.last, id)
                })
                .max()
        };
        let exact = a.id(prev).and_then(|p| best(&a.seq, p));
        let pick = exact.or_else(|| head(prev).and_then(|h| best(&a.seq_head, a.id(&h)?)))?;
        Some(a.strings[pick.3 as usize].to_string())
    }

    /// Record one executed command: update the aggregates and append
    /// the row. `Ok(false)` means the command was dropped as ignored. An
    /// ignored predecessor is cleared rather than stored, so it never
    /// survives as the key of a sequence. A write error leaves the
    /// in-memory record in place and is returned for the caller to log.
    pub fn record(&self, mut e: Entry) -> std::io::Result<bool> {
        if is_ignored(&e.command) {
            return Ok(false);
        }
        if is_ignored(&e.prev) {
            e.prev.clear();
        }
        let _io = self
            .io
            .lock()
            .map_err(|_| std::io::Error::other("history lock poisoned"))?;
        let rows = {
            let mut agg = self
                .agg
                .lock()
                .map_err(|_| std::io::Error::other("history lock poisoned"))?;
            agg.add(&e);
            agg.rows
        };
        if let Some(path) = &self.path {
            let _lock = FileLock::acquire(path)?;
            append_unlocked(path, &e)?;
            if rows > ROW_CAP {
                self.compact(path)?;
            }
        }
        Ok(true)
    }

    /// Keep the newest [`ROW_KEEP`] rows and rebuild the aggregates from
    /// exactly what the file now holds. The caller holds the [`FileLock`].
    fn compact(&self, path: &Path) -> std::io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        let rows: Vec<&str> = text.lines().filter(|l| parse_row(l).is_some()).collect();
        let kept = &rows[rows.len().saturating_sub(ROW_KEEP)..];
        let mut out = kept.join("\n");
        out.push('\n');
        write_new_private(path, &out)?;
        let mut agg = Aggregates::default();
        for e in kept.iter().filter_map(|l| parse_row(l)) {
            agg.add(&e);
        }
        if let Ok(mut cur) = self.agg.lock() {
            agg.generation = cur.generation + 1;
            *cur = agg;
        }
        Ok(())
    }

    /// Seed the history from a zsh history file, once. The imported rows
    /// go *before* whatever is already recorded — the first command of a
    /// new shell can reach the daemon before the import it fired at the
    /// first prompt. Imported rows are the only ones with an empty cwd
    /// (every recorded command carries `$PWD`), so a history holding any
    /// is already seeded and the call is a no-op. Returns how many
    /// commands were imported.
    pub fn import_zsh_history(&self, histfile: &Path) -> std::io::Result<usize> {
        if !std::fs::metadata(histfile)?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let _io = self
            .io
            .lock()
            .map_err(|_| std::io::Error::other("history lock poisoned"))?;
        let _lock = match &self.path {
            Some(path) => Some(FileLock::acquire(path)?),
            None => None,
        };
        // Read the file, not the aggregates: another process may have
        // written since this store loaded.
        let existing: Vec<Entry> = match &self.path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text.lines().filter_map(parse_row).collect(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e),
            },
            None => Vec::new(),
        };
        if existing.iter().any(|e| e.cwd.is_empty()) {
            return Ok(0);
        }
        let bytes = read_tail(histfile, IMPORT_MAX_BYTES)?;
        let fallback_ts = std::fs::metadata(histfile)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or_else(now_unix);
        let imported = parse_zsh_history(&unmetafy(&bytes), fallback_ts);
        if imported.is_empty() {
            return Ok(0);
        }
        let room = ROW_KEEP.saturating_sub(existing.len());
        let kept = &imported[imported.len().saturating_sub(room)..];
        if kept.is_empty() {
            return Ok(0);
        }
        let all: Vec<&Entry> = kept.iter().chain(existing.iter()).collect();
        if let Some(path) = &self.path {
            let mut out = String::new();
            for e in &all {
                out.push_str(&format_row(e));
                out.push('\n');
            }
            write_new_private(path, &out)?;
        }
        let mut agg = Aggregates::default();
        for e in &all {
            agg.add(e);
        }
        if let Ok(mut cur) = self.agg.lock() {
            agg.generation = cur.generation + 1;
            *cur = agg;
        }
        Ok(kept.len())
    }
}

/// Exclusive advisory lock on `<history>.lock`, held for one write. Same
/// shape as `nervd.start.lock`: a sibling file in the cache dir, so the
/// uninstall sweep removes it.
struct FileLock {
    _file: std::fs::File,
}

impl FileLock {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let f = open_private(Path::new(&name), true)?;
        // SAFETY: flock on a descriptor this function owns; released when
        // the file closes.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { _file: f })
    }
}

/// Append one row under the file lock, creating the file `0600` if
/// needed. The CLI's path when no daemon answers: the daemon reads the
/// row at its next load.
pub fn append_row(path: &Path, e: &Entry) -> std::io::Result<()> {
    let _lock = FileLock::acquire(path)?;
    append_unlocked(path, e)
}

fn append_unlocked(path: &Path, e: &Entry) -> std::io::Result<()> {
    let mut f = open_private(path, true)?;
    // One write call per row, so concurrent appenders cannot interleave
    // inside a line.
    f.write_all(format!("{}\n", format_row(e)).as_bytes())
}

fn open_private(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(append)
        .write(true)
        .truncate(!append)
        .mode(0o600)
        .open(path)?;
    // `mode` applies only on creation; a file that predates it is fixed
    // here so it cannot stay world-readable.
    if let Ok(meta) = f.metadata() {
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(f)
}

/// Write a whole file through a `0600` temp file and a rename.
fn write_new_private(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The pid keeps two processes' temp files apart; the rename itself
    // runs under the file lock.
    let tmp = path.with_extension(format!("tsv.{}.nerv-tmp", std::process::id()));
    let res = open_private(&tmp, false)
        .and_then(|mut f| {
            f.write_all(content.as_bytes())?;
            f.sync_all()
        })
        .and_then(|_| std::fs::rename(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

pub fn format_row(e: &Entry) -> String {
    let expanded = if e.expanded == e.command {
        ""
    } else {
        &e.expanded
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        e.ts,
        e.exit,
        escape(&e.cwd),
        escape(&e.prev),
        escape(&e.command),
        escape(expanded)
    )
}

pub fn parse_row(line: &str) -> Option<Entry> {
    let mut f = line.split('\t');
    let ts = f.next()?.parse().ok()?;
    let exit = f.next()?.parse().ok()?;
    let cwd = unescape(f.next()?);
    let prev = unescape(f.next()?);
    let command = unescape(f.next()?);
    let expanded = f.next().map(unescape).unwrap_or_default();
    if is_ignored(&command) {
        return None;
    }
    Some(Entry {
        ts,
        exit,
        cwd,
        prev,
        command,
        expanded,
    })
}

/// zsh writes history "metafied": a byte in 0x83..=0x9f and a few others
/// is stored as 0x83 (Meta) followed by the byte XOR 0x20. Reading the
/// file as-is turns a Korean command into mojibake.
/// The last `max` bytes of a file, from the first line that starts inside
/// them: a line cut in half is not a command that was run.
fn read_tail(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len <= max {
        let mut bytes = Vec::with_capacity(len as usize);
        f.read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    f.seek(SeekFrom::Start(len - max))?;
    let mut bytes = Vec::with_capacity(max as usize);
    f.take(max).read_to_end(&mut bytes)?;
    let start = bytes
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |i| i + 1);
    bytes.drain(..start);
    Ok(bytes)
}

pub fn unmetafy(bytes: &[u8]) -> String {
    const META: u8 = 0x83;
    let mut out = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter();
    while let Some(&b) = it.next() {
        if b == META {
            if let Some(&n) = it.next() {
                out.push(n ^ 0x20);
            }
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse plain and `EXTENDED_HISTORY` (`: <ts>:<dur>;<cmd>`) lines. A
/// line ending in a backslash continues onto the next (zsh's encoding of
/// a multi-line command). Consecutive commands become sequence pairs;
/// the directory is unknown, so it stays empty.
pub fn parse_zsh_history(text: &str, fallback_ts: u64) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut pending: Option<(u64, String)> = None;
    let mut prev = String::new();
    let mut push = |ts: u64, cmd: String, out: &mut Vec<Entry>| {
        if is_ignored(&cmd) {
            prev.clear();
            return;
        }
        out.push(Entry {
            ts,
            exit: 0,
            cwd: String::new(),
            prev: std::mem::replace(&mut prev, cmd.clone()),
            command: cmd,
            expanded: String::new(),
        });
    };
    for line in text.lines() {
        let (ts, body) = match pending.take() {
            Some((ts, mut acc)) => {
                acc.push('\n');
                acc.push_str(line);
                (ts, acc)
            }
            None => match parse_extended(line) {
                Some((ts, cmd)) => (ts, cmd.to_string()),
                None => (fallback_ts, line.to_string()),
            },
        };
        if let Some(stripped) = body.strip_suffix('\\') {
            pending = Some((ts, stripped.to_string()));
            continue;
        }
        push(ts, body, &mut out);
    }
    if let Some((ts, acc)) = pending {
        push(ts, acc, &mut out);
    }
    out
}

fn parse_extended(line: &str) -> Option<(u64, &str)> {
    let rest = line.strip_prefix(": ")?;
    let (meta, cmd) = rest.split_once(';')?;
    let (ts, _dur) = meta.split_once(':')?;
    Some((ts.trim().parse().ok()?, cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nerv-history-{}-{}-{name}",
            std::process::id(),
            now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.tsv")
    }

    impl HistoryStore {
        fn record_ok(&self, e: Entry) -> bool {
            self.record(e).unwrap()
        }
    }

    fn entry(cmd: &str, cwd: &str, prev: &str) -> Entry {
        Entry {
            ts: 1_000,
            exit: 0,
            cwd: cwd.into(),
            prev: prev.into(),
            command: cmd.into(),
            expanded: String::new(),
        }
    }

    /// A file past the bound is read from its tail, starting at a whole
    /// line; a non-regular file is refused before anything is read.
    #[test]
    fn import_reads_a_bounded_tail_of_regular_files_only() {
        let path = tmp("tail").with_file_name("zsh_history");
        std::fs::write(&path, "echo first\necho second\necho third\n").unwrap();
        assert_eq!(
            read_tail(&path, 1024).unwrap(),
            b"echo first\necho second\necho third\n"
        );
        // 14 bytes back lands inside `echo second`: that half line goes.
        assert_eq!(read_tail(&path, 14).unwrap(), b"echo third\n");
        let store = HistoryStore::load(&tmp("tail-dir"));
        let err = store
            .import_zsh_history(Path::new("/dev/null"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn record_appends_one_row_and_updates_aggregates() {
        let path = tmp("append");
        let store = HistoryStore::load(&path);
        assert!(store.record_ok(entry("make test", "/a", "")));
        assert!(store.record_ok(entry("make build", "/a", "make test")));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert_eq!(store.stat("make test").unwrap().count, 1);
        assert_eq!(store.runs_in("make test", "/a"), 1);
        assert_eq!(store.followed("make test", "make build"), 1);

        let reloaded = HistoryStore::load(&path);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.followed("make test", "make build"), 1);
    }

    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp("perm");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        HistoryStore::load(&path)
            .record(entry("ls", "/", ""))
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn ignored_commands_leave_no_row_and_no_sequence_key() {
        let path = tmp("ignored");
        let store = HistoryStore::load(&path);
        assert!(!store.record_ok(entry(" export TOKEN=abc", "/", "")));
        assert!(!store.record_ok(entry("\tsecret", "/", "")));
        assert!(!store.record_ok(entry("   ", "/", "")));
        // A predecessor that slipped through with its leading space is
        // cleared, not stored as a sequence key.
        assert!(store.record_ok(entry("ls", "/", " export TOKEN=abc")));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("TOKEN"), "{text}");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn tabs_newlines_and_backslashes_round_trip() {
        let path = tmp("escape");
        let store = HistoryStore::load(&path);
        let cmd = "printf 'a\\tb'\t|\nfor x in 1; do\n  echo \\$x\ndone";
        let mut e = entry(cmd, "/dir\twith tab", "");
        e.expanded = "expanded\nform".into();
        store.record(e.clone()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let back = parse_row(text.lines().next().unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn compaction_keeps_the_newest_rows_and_matching_aggregates() {
        let path = tmp("compact");
        let mut body = String::new();
        for i in 0..ROW_CAP {
            body.push_str(&format_row(&entry(&format!("cmd{i}"), "/", "")));
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();
        let store = HistoryStore::load(&path);
        assert_eq!(store.len(), ROW_CAP);
        store.record(entry("newest", "/", "")).unwrap();
        assert_eq!(store.len(), ROW_KEEP);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), ROW_KEEP);
        assert!(store.stat("newest").is_some());
        assert!(store.stat("cmd0").is_none());
        let first_kept = ROW_CAP + 1 - ROW_KEEP;
        assert!(store.stat(&format!("cmd{first_kept}")).is_some());
        assert!(store.stat(&format!("cmd{}", first_kept - 1)).is_none());
    }

    #[test]
    fn corrupt_rows_are_skipped() {
        let path = tmp("corrupt");
        std::fs::write(
            &path,
            "garbage\n1\t0\t/\t\tls\t\nnot\ta\trow\n2\tx\t/\t\tpwd\t\n",
        )
        .unwrap();
        let store = HistoryStore::load(&path);
        assert_eq!(store.len(), 1);
        assert!(store.stat("ls").is_some());
    }

    #[test]
    fn imports_extended_plain_and_multiline_history() {
        let text = ": 1700000000:0;git add .\n\
                    : 1700000005:0;git commit -m wip\n\
                    : 1700000010:0;for f in *; do\\\n  echo $f\\\ndone\n \
                    leading space is ignored\n\
                    plain line\n";
        let es = parse_zsh_history(text, 42);
        let cmds: Vec<&str> = es.iter().map(|e| e.command.as_str()).collect();
        assert_eq!(
            cmds,
            [
                "git add .",
                "git commit -m wip",
                "for f in *; do\n  echo $f\ndone",
                "plain line"
            ]
        );
        assert_eq!(es[1].prev, "git add .");
        assert_eq!(es[1].ts, 1_700_000_005);
        // The ignored line broke the chain.
        assert_eq!(es[3].prev, "");
        assert_eq!(es[3].ts, 42);
    }

    #[test]
    fn metafied_korean_history_imports_intact() {
        let original = "echo 한글 테스트";
        // zsh metafies bytes 0x83..=0x9f (and NUL etc): emit Meta + b^0x20.
        let mut meta = Vec::new();
        for &b in original.as_bytes() {
            if b == 0 || (0x83..=0x9f).contains(&b) {
                meta.push(0x83);
                meta.push(b ^ 0x20);
            } else {
                meta.push(b);
            }
        }
        assert_ne!(meta, original.as_bytes(), "fixture must exercise Meta");
        assert_eq!(unmetafy(&meta), original);

        let path = tmp("korean");
        let hist = path.with_file_name("zsh_history");
        let mut file = b": 1700000000:0;".to_vec();
        file.extend_from_slice(&meta);
        file.push(b'\n');
        std::fs::write(&hist, file).unwrap();
        let store = HistoryStore::load(&path);
        assert_eq!(store.import_zsh_history(&hist).unwrap(), 1);
        assert!(store.stat(original).is_some());
        assert!(HistoryStore::load(&path).stat(original).is_some());
    }

    #[test]
    fn import_runs_once_and_keeps_rows_recorded_before_it() {
        let path = tmp("import-once");
        let hist = path.with_file_name("zsh_history");
        std::fs::write(&hist, "ls\npwd\n").unwrap();
        let store = HistoryStore::load(&path);
        // The shell's first command beat the import to the daemon.
        store.record(entry("make", "/w", "")).unwrap();
        assert_eq!(store.import_zsh_history(&hist).unwrap(), 2);
        assert_eq!(store.import_zsh_history(&hist).unwrap(), 0);
        // A second store (another process) sees the seed on disk.
        assert_eq!(
            HistoryStore::load(&path).import_zsh_history(&hist).unwrap(),
            0
        );
        let cmds: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter_map(parse_row)
            .map(|e| e.command)
            .collect();
        assert_eq!(cmds, ["ls", "pwd", "make"]);
        assert_eq!(store.len(), 3);
        assert_eq!(store.followed("ls", "pwd"), 1);
    }

    #[test]
    fn appends_from_another_process_survive_an_import() {
        // A no-daemon CLI append lands between this store's load and its
        // import; the import must fold it in, not rename over it.
        let path = tmp("import-race");
        let hist = path.with_file_name("zsh_history");
        std::fs::write(&hist, "ls\n").unwrap();
        let store = HistoryStore::load(&path);
        append_row(&path, &entry("from-cli", "/w", "")).unwrap();
        store.import_zsh_history(&hist).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("from-cli"), "{text}");
        assert!(store.stat("from-cli").is_some());
    }

    fn at(cmd: &str, cwd: &str, prev: &str, ts: u64) -> Entry {
        Entry {
            ts,
            ..entry(cmd, cwd, prev)
        }
    }

    const NOW: u64 = 1_700_000_000;

    #[test]
    fn ghost_extends_what_was_typed_and_nothing_else() {
        let store = HistoryStore::empty();
        for c in ["git status", "git", "ls -la", "echo a\\\nb", "gitk"] {
            store.record(at(c, "/", "", NOW)).unwrap();
        }
        store.record(at("echo multi\nline", "/", "", NOW)).unwrap();
        assert_eq!(store.ghost("git s", "/", "").as_deref(), Some("git status"));
        // The exact text is not an extension of itself.
        assert_eq!(store.ghost("git status", "/", ""), None);
        assert_eq!(store.ghost("xyz", "/", ""), None);
        assert_eq!(store.ghost("", "/", ""), None);
        // A multi-line command is never a ghost, nor one with a tab.
        assert_eq!(store.ghost("echo m", "/", ""), None);
        store.record(at("printf 'a\tb'", "/", "", NOW)).unwrap();
        assert_eq!(store.ghost("printf", "/", ""), None);
    }

    #[test]
    fn directory_affinity_flips_the_ghost() {
        let store = HistoryStore::empty();
        for _ in 0..3 {
            store.record(at("make test", "/a", "", NOW)).unwrap();
            store.record(at("make build", "/b", "", NOW)).unwrap();
        }
        assert_eq!(store.ghost("make ", "/a", "").as_deref(), Some("make test"));
        assert_eq!(
            store.ghost("make ", "/b", "").as_deref(),
            Some("make build")
        );
    }

    #[test]
    fn sequence_beats_raw_frequency() {
        let store = HistoryStore::empty();
        // `git push` is run far more often overall …
        for _ in 0..6 {
            store.record(at("git push", "/", "", NOW)).unwrap();
        }
        // … but after `git add .` the user commits.
        for _ in 0..2 {
            store
                .record(at("git commit -m wip", "/", "git add .", NOW))
                .unwrap();
        }
        assert_eq!(store.ghost("git ", "/", "").as_deref(), Some("git push"));
        assert_eq!(
            store.ghost("git ", "/", "git add .").as_deref(),
            Some("git commit -m wip")
        );
        // Two words with doubled whitespace are still a whole command, not
        // a head that double-counts the exact-prev term.
        assert_eq!(head("git  add"), None);
        assert_eq!(head("git add ."), Some("git add".into()));
        // A different `git add` still counts through the command head.
        assert_eq!(
            store.ghost("git ", "/", "git add src/x.rs").as_deref(),
            Some("git commit -m wip")
        );
    }

    #[test]
    fn recency_decays_frequency() {
        let store = HistoryStore::empty();
        let month = 30 * 86_400;
        for _ in 0..10 {
            store
                .record(at("npm run old", "/", "", NOW - month))
                .unwrap();
        }
        store.record(at("npm run new", "/", "", NOW)).unwrap();
        assert_eq!(
            store.ghost("npm run ", "/", "").as_deref(),
            Some("npm run new")
        );
        // Within the week, frequency still wins.
        let store = HistoryStore::empty();
        for _ in 0..10 {
            store
                .record(at("npm run often", "/", "", NOW - 86_400))
                .unwrap();
        }
        store.record(at("npm run once", "/", "", NOW)).unwrap();
        assert_eq!(
            store.ghost("npm run ", "/", "").as_deref(),
            Some("npm run often")
        );
    }

    #[test]
    fn frecency_key_ratio_matches_frecency() {
        let a = Stat {
            count: 10,
            last: NOW - 3 * 86_400,
            ..Stat::default()
        };
        let b = Stat {
            count: 2,
            last: NOW,
            ..Stat::default()
        };
        let direct = frecency(a.count, a.last, NOW) / frecency(b.count, b.last, NOW);
        let keyed = (a.frecency_key() - b.frecency_key()).exp();
        assert!((direct - keyed).abs() < 1e-9, "{direct} vs {keyed}");
    }

    #[test]
    fn predict_needs_a_repeated_habit() {
        let store = HistoryStore::empty();
        store
            .record(at("git commit -m a", "/", "git add .", NOW))
            .unwrap();
        // Seen once: no prediction.
        assert_eq!(store.predict("git add .", "/"), None);
        store
            .record(at("git commit -m b", "/", "git add .", NOW))
            .unwrap();
        store
            .record(at("git commit -m b", "/", "git add .", NOW))
            .unwrap();
        assert_eq!(
            store.predict("git add .", "/").as_deref(),
            Some("git commit -m b")
        );
        // The head of a different `git add` finds the same habit.
        assert_eq!(
            store.predict("git add src/x.rs", "/").as_deref(),
            Some("git commit -m b")
        );
        assert_eq!(store.predict("ls", "/"), None);
        assert_eq!(store.predict("", "/"), None);
    }

    #[test]
    fn predict_breaks_ties_by_directory() {
        let store = HistoryStore::empty();
        for _ in 0..2 {
            store.record(at("make test", "/a", "make", NOW)).unwrap();
            store.record(at("make run", "/b", "make", NOW + 1)).unwrap();
        }
        assert_eq!(store.predict("make", "/a").as_deref(), Some("make test"));
        assert_eq!(store.predict("make", "/b").as_deref(), Some("make run"));
    }

    #[test]
    fn token_signals_collect_the_next_word_by_expanded_form() {
        let store = HistoryStore::empty();
        store
            .record(at("git checkout main", "/r", "", NOW))
            .unwrap();
        store.record(at("git checkout dev", "/r", "", NOW)).unwrap();
        store.record(at("git  status", "/other", "", NOW)).unwrap();
        let mut aliased = at("g checkout main", "/r", "", NOW);
        aliased.expanded = "git checkout main".into();
        store.record(aliased).unwrap();
        store.record(at("gitk --all", "/r", "", NOW)).unwrap();

        let sig = store.token_signals(&["git"], "/r", "");
        let co = sig.get("checkout").unwrap();
        // Three runs, two of the same text (typed and via the alias).
        assert_eq!((co.count, co.here, co.in_dirs), (3, 3, 3));
        let st = sig.get("status").unwrap();
        assert_eq!((st.count, st.here, st.in_dirs), (1, 0, 1));
        // `gitk` is a different command word, not a `git` row.
        assert!(sig.get("--all").is_none());
        assert_eq!(sig.tokens.len(), 2);

        // A new run is seen at once, cache or not.
        store.record(at("git status", "/r", "", NOW)).unwrap();
        assert_eq!(
            store
                .token_signals(&["git"], "/r", "")
                .get("status")
                .unwrap()
                .count,
            2
        );
        let deeper = store.token_signals(&["git", "checkout"], "/r", "");
        assert_eq!(deeper.get("main").unwrap().count, 2);
        assert!(store.token_signals(&[], "/r", "").tokens.is_empty());
        // The typed form of the word is kept for rows the spec lacks.
        assert_eq!(deeper.get("main").unwrap().word, "main");
    }

    #[test]
    fn token_signals_count_what_followed_the_previous_command() {
        let store = HistoryStore::empty();
        for _ in 0..2 {
            store
                .record(at("git commit -m wip", "/", "git add .", NOW))
                .unwrap();
        }
        store.record(at("git push", "/", "", NOW)).unwrap();
        let after_add = store.token_signals(&["git"], "/", "git add src/x.rs");
        let commit = after_add.get("commit").unwrap();
        // Not the same `git add`, but the same head.
        assert_eq!((commit.after_prev, commit.after_head), (0, 2));
        let exact = store.token_signals(&["git"], "/", "git add .");
        assert_eq!(exact.get("commit").unwrap().after_prev, 2);
        assert_eq!(exact.get("push").unwrap().after_prev, 0);
    }

    #[test]
    fn token_key_normalises_options_and_dirs() {
        assert_eq!(token_key("--mode="), "--mode");
        assert_eq!(token_key("--mode=fast"), "--mode");
        assert_eq!(token_key("src/"), "src");
        assert_eq!(token_key("checkout "), "checkout");
        assert_eq!(token_key("/"), "/");
        assert_eq!(token_key("-v"), "-v");
    }

    #[test]
    fn command_not_found_is_never_offered() {
        let store = HistoryStore::empty();
        let mut typo = at("gti status", "/", "", NOW);
        typo.exit = EXIT_NOT_FOUND;
        store.record(typo).unwrap();
        assert_eq!(store.ghost("gti", "/", ""), None);
        assert_eq!(store.len(), 1);
    }

    /// Resident growth of loading a full 100k-row history, measured in a
    /// process of its own (`cargo test … history_100k_rss -- --ignored`):
    /// peak RSS is process-wide, so parallel tests would pollute it.
    #[test]
    #[ignore]
    fn history_100k_rss() {
        fn peak_rss() -> u64 {
            let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
            // macOS reports bytes, Linux kilobytes.
            if cfg!(target_os = "macos") {
                ru.ru_maxrss as u64
            } else {
                ru.ru_maxrss as u64 * 1024
            }
        }
        let path = tmp("rss");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..ROW_CAP {
                let e = Entry {
                    ts: 1_700_000_000 + i as u64,
                    exit: 0,
                    cwd: format!("/Users/me/workspace/project-{}", i % 40),
                    prev: format!("git commit -m 'change number {}'", i.wrapping_sub(1)),
                    command: format!("git commit -m 'change number {i}'"),
                    expanded: String::new(),
                };
                writeln!(f, "{}", format_row(&e)).unwrap();
            }
        }
        let before = peak_rss();
        let store = HistoryStore::load(&path);
        let after = peak_rss();
        assert_eq!(store.len(), ROW_CAP);
        // Worst keystroke: every one of the 100k commands extends `git c`,
        // and every one followed a `git commit …` (the head term). Best of
        // five — the first call also pays for faulting the tables in.
        let mut ghost_ms = f64::MAX;
        let mut g = None;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            g = store.ghost(
                "git c",
                "/Users/me/workspace/project-3",
                "git commit -m 'x'",
            );
            ghost_ms = ghost_ms.min(t.elapsed().as_secs_f64() * 1e3);
        }
        assert!(g.is_some());
        eprintln!("history_100k_rss: ghost over 100k candidates {ghost_ms:.2} ms");
        // The popup's scan: every one of the 100k commands follows `git`.
        // The first keystroke after a space pays it; the rest of the word
        // hits the cache.
        let t = std::time::Instant::now();
        let sig = store.token_signals(&["git"], "/Users/me/workspace/project-3", "");
        let cold_ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(sig.tokens.len(), 1);
        let t = std::time::Instant::now();
        let again = store.token_signals(&["git"], "/Users/me/workspace/project-3", "");
        let warm_ms = t.elapsed().as_secs_f64() * 1e3;
        assert!(Arc::ptr_eq(&sig, &again), "same words, same cwd: cached");
        eprintln!(
            "history_100k_rss: token_signals over 100k commands {cold_ms:.2} ms cold, {warm_ms:.3} ms cached"
        );
        // Timings are reported, not asserted: under parallel load a wall
        // clock bound fails on its own. The cache is what `ptr_eq` pins.
        // Measured 2.8–4.5 ms (release, M-series); the bound leaves room
        // for a loaded machine and still sits well inside the 25 ms budget.
        assert!(ghost_ms < 10.0, "ghost took {ghost_ms:.2} ms");
        let grew = after.saturating_sub(before);
        eprintln!("history_100k_rss: +{} MiB", grew >> 20);
        assert!(
            grew < 40 << 20,
            "loading 100k rows grew RSS by {} MiB",
            grew >> 20
        );
    }
}
