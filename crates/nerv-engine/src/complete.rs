//! complete — the high-level completion pipeline.
//!
//! Plumbing between `shell_parser` (tokenize the input line),
//! `spec_loader` (find the right Spec for the binary name), and
//! `spec_parser` (run the matcher to get cursor context), then emit
//! ranked [`Suggestion`]s based on the cursor context.
//!
//! v0.6 / M0-6 chunk 4 + M1 lazy: SpecRegistry now reads specs from
//! disk on first lookup and caches Arc<Spec> in a RwLock. Eager bulk
//! load is gone — startup is O(1), memory grows with use.
//!
//! Refs: PLAN.md §10 M0-6, docs/first-5-min.md §1-5

use crate::config::MatchMode;
use crate::ipc::{Suggestion, SuggestionKind};
use crate::spec_loader::{SpecLoadError, load_spec_file};
use crate::spec_parser::{
    Annotation, CursorContext, Spec, Subcommand, TokenKind, find_subcommand, parse_arguments,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Lazy spec set, keyed by binary name.
///
/// On lookup, checks the in-memory cache first; on miss, reads
/// `<dir>/<name>.json` from disk and inserts the parsed spec
/// (or `None` for a negative cache entry) into the cache so the
/// next lookup is O(1).
///
/// Hot-reload: each positive cache entry tracks the file's mtime.
/// On lookup, the file is stat'd; if mtime has advanced (user
/// reinstalled / regenerated the spec), the cache entry is dropped
/// and the spec is re-read. Adds ~1µs per lookup on top of the
/// HashMap hit (cheap compared to even the fastest UDS roundtrip).
pub struct SpecRegistry {
    dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, CacheEntry>>,
    /// Spec stems (binary names) marked dirty by the FS watcher. Drained
    /// at lookup-time so any cached entry gets re-read from disk on the
    /// very next call. `None` when no watcher is active (e.g. empty
    /// registry, dir doesn't exist, or notify failed to start).
    pending_invalidations: Option<Arc<std::sync::Mutex<std::collections::HashSet<String>>>>,
    /// Held to keep the watcher thread alive for the registry's lifetime.
    /// Dropping the watcher stops the FS event stream.
    _watcher: Option<Box<dyn notify::Watcher + Send + Sync>>,
}

impl std::fmt::Debug for SpecRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpecRegistry")
            .field("dir", &self.dir)
            .field("cache", &self.cache)
            .field("watcher_active", &self._watcher.is_some())
            .finish()
    }
}

impl Default for SpecRegistry {
    fn default() -> Self {
        Self {
            dir: None,
            cache: RwLock::new(HashMap::new()),
            pending_invalidations: None,
            _watcher: None,
        }
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    /// Last-known mtime of the on-disk file. `None` = negative
    /// cache entry (file didn't exist last time we looked).
    mtime: Option<std::time::SystemTime>,
    /// `None` for negative entries OR parse failures (don't keep
    /// retrying a broken file every keystroke).
    spec: Option<Arc<Spec>>,
}

impl SpecRegistry {
    /// Empty registry (useful for tests that build Specs in code).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a registry rooted at `dir`. Disk reads are lazy.
    ///
    /// Also starts a filesystem watcher (FSEvents on macOS) that
    /// invalidates the cache when spec files change on disk —
    /// `build-specs` reinstall is picked up on the next lookup without
    /// restarting the daemon. The mtime check in `lookup` stays as a
    /// belt-and-suspenders fallback if the watcher fails or drops events.
    pub fn at_dir(dir: &Path) -> Self {
        let pending = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let watcher = start_spec_watcher(dir, pending.clone());
        Self {
            dir: Some(dir.to_path_buf()),
            cache: RwLock::new(HashMap::new()),
            pending_invalidations: Some(pending),
            _watcher: watcher,
        }
    }

    /// Build a registry rooted at `dir` and eagerly scan it for parse
    /// errors. Useful at daemon startup so problems show up in logs
    /// without waiting for a user keystroke. Returns the registry +
    /// every error encountered during the scan; positive results are
    /// kept in the cache so subsequent lookups are O(1). FS watcher
    /// is spawned the same way as `at_dir`.
    pub fn load_dir(dir: &Path) -> (Self, Vec<SpecLoadError>) {
        let registry = Self::at_dir(dir);
        let mut errors = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return (registry, errors);
        };
        let mut cache = registry.cache.write().expect("cache poisoned");
        for entry in entries.flatten() {
            let path = entry.path();
            // Accept both plain `.json` and gzipped `.json.gz`. Earlier
            // versions skipped the latter, causing `nerv doctor` to
            // report `0 specs loaded` against a populated `.json.gz`
            // cache.
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_json = file_name.ends_with(".json");
            let is_gz = file_name.ends_with(".json.gz");
            if !is_json && !is_gz {
                continue;
            }
            let mtime = path.metadata().and_then(|m| m.modified()).ok();
            match load_spec_file(&path) {
                Ok(spec) => {
                    let key = if !spec.name.is_empty() {
                        spec.name.clone()
                    } else if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        stem.to_string()
                    } else {
                        continue;
                    };
                    cache.insert(
                        key,
                        CacheEntry {
                            mtime,
                            spec: Some(Arc::new(spec)),
                        },
                    );
                }
                Err(e) => errors.push(e),
            }
        }
        drop(cache);
        (registry, errors)
    }

    /// Resolve a spec by binary name, loading from disk on first hit
    /// or when the file's mtime has advanced past the cached value.
    /// Returns `None` if the spec doesn't exist or failed to parse.
    ///
    /// **Lock-poison policy**: every `cache.read()` / `cache.write()`
    /// in this module discards `PoisonError` via `.ok()` /
    /// `if let Ok(...)`. The fallback path is graceful degradation —
    /// on a poisoned cache, lookup bypasses the cache and re-reads
    /// from disk on every call (slower, but correct). A poison can
    /// only fire if a worker panics while holding the lock; that
    /// panic itself surfaces via the daemon's stderr already.
    pub fn lookup(&self, name: &str) -> Option<Arc<Spec>> {
        // Drain any FS-watcher invalidations queued since the last
        // lookup. Each drained stem evicts its cache entry so the
        // next read goes back to disk.
        self.drain_invalidations();
        // Fast path: cache hit + mtime unchanged. Poisoned read →
        // None → falls through to load_from_disk (no incorrectness).
        let cached = self.cache.read().ok().and_then(|c| c.get(name).cloned());
        if let Some(entry) = cached {
            let disk_mtime = self.disk_mtime(name);
            if entry.mtime == disk_mtime {
                return entry.spec;
            }
            // Mtime advanced (or file gone) — fall through to reload.
        }
        let (mtime, spec) = self.load_from_disk(name);
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(
                name.to_string(),
                CacheEntry {
                    mtime,
                    spec: spec.clone(),
                },
            );
        }
        spec
    }

    /// stat() the file backing `name` (plain or .gz form) and return
    /// its mtime. `None` if the file doesn't exist or stat fails.
    fn disk_mtime(&self, name: &str) -> Option<std::time::SystemTime> {
        let dir = self.dir.as_ref()?;
        let plain = dir.join(format!("{name}.json"));
        if let Ok(m) = plain.metadata() {
            return m.modified().ok();
        }
        let gz = dir.join(format!("{name}.json.gz"));
        if let Ok(m) = gz.metadata() {
            return m.modified().ok();
        }
        None
    }

    fn load_from_disk(&self, name: &str) -> (Option<std::time::SystemTime>, Option<Arc<Spec>>) {
        let Some(dir) = self.dir.as_ref() else {
            return (None, None);
        };
        // Prefer plain JSON for human inspection; fall back to gzipped
        // form (build-time compressed cache).
        let plain = dir.join(format!("{name}.json"));
        if plain.exists() {
            let mtime = plain.metadata().and_then(|m| m.modified()).ok();
            let spec = load_spec_file(&plain).ok().map(Arc::new);
            return (mtime, spec);
        }
        let gz = dir.join(format!("{name}.json.gz"));
        if gz.exists() {
            let mtime = gz.metadata().and_then(|m| m.modified()).ok();
            let spec = load_spec_file(&gz).ok().map(Arc::new);
            return (mtime, spec);
        }
        (None, None)
    }

    /// Move every queued FS-watcher invalidation into the cache:
    /// each stem maps to a cache entry that gets dropped, so the
    /// next `lookup(stem)` re-reads the spec file. No-op when no
    /// watcher is active.
    fn drain_invalidations(&self) {
        let Some(pending) = self.pending_invalidations.as_ref() else {
            return;
        };
        let Ok(mut pending) = pending.lock() else {
            return;
        };
        if pending.is_empty() {
            return;
        }
        let names: Vec<String> = pending.drain().collect();
        drop(pending);
        if let Ok(mut cache) = self.cache.write() {
            for name in &names {
                cache.remove(name);
            }
        }
    }

    /// Insert a spec into the cache directly. Used by tests that
    /// build Specs in code and by future hot-reload paths. The
    /// inserted entry has no mtime — so `lookup` will re-stat and
    /// potentially evict it if the dir is configured AND a file with
    /// the same name exists on disk.
    pub fn insert(&self, spec: Spec) {
        let key = spec.name.clone();
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(
                key,
                CacheEntry {
                    mtime: None,
                    spec: Some(Arc::new(spec)),
                },
            );
        }
    }

    /// Count of positive cache entries. Does not include negative hits
    /// or specs that have not been looked up yet (lazy).
    pub fn len(&self) -> usize {
        self.cache
            .read()
            .map(|c| c.values().filter(|v| v.spec.is_some()).count())
            .unwrap_or(0)
    }

    /// `true` when no positive specs are cached. Lazy registries
    /// can report empty until first lookup — use `dir_listing` to
    /// see what is actually available on disk.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Names currently in the in-memory positive cache. Does not
    /// reflect disk contents — see [`Self::dir_listing`] for that.
    pub fn cached_names(&self) -> Vec<String> {
        self.cache
            .read()
            .map(|c| {
                c.iter()
                    .filter(|(_, v)| v.spec.is_some())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Walk the spec dir and return file-stem names of every
    /// `*.json` or `*.json.gz` present. Used by `nerv spec list` so
    /// the table reflects what's installed even before any lookup.
    /// Deduplicates if both forms exist (plain wins).
    pub fn dir_listing(&self) -> Vec<String> {
        let Some(dir) = self.dir.as_ref() else {
            return self.cached_names();
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut seen = std::collections::BTreeSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let stem = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) if s.ends_with(".json.gz") => &s[..s.len() - 8],
                Some(s) if s.ends_with(".json") => &s[..s.len() - 5],
                _ => continue,
            };
            seen.insert(stem.to_string());
        }
        seen.into_iter().collect()
    }
}

/// Spawn a filesystem watcher over `dir` and forward changed-file
/// events into `pending` as spec stems. Returns `None` when the
/// watcher fails to start (the registry stays correct via mtime
/// polling so this is a soft failure).
fn start_spec_watcher(
    dir: &Path,
    pending: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
) -> Option<Box<dyn notify::Watcher + Send + Sync>> {
    use notify::{EventKind, RecursiveMode, Watcher};

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        // Only Create / Modify / Remove signal an actual cache
        // invalidation. Access / Metadata events would needlessly
        // drop entries (e.g. when `nerv doctor` stat's the file).
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }
        let Ok(mut q) = pending.lock() else {
            return;
        };
        for path in &event.paths {
            if let Some(stem) = spec_stem_from_path(path) {
                q.insert(stem);
            }
        }
    })
    .ok()?;
    // NonRecursive: the specs directory is flat (Caches/nerv/specs).
    watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(Box::new(watcher))
}

/// Pull the spec stem out of a watched path, accepting both `<name>.json`
/// and `<name>.json.gz`. Returns `None` for paths that don't look like
/// spec files (rejects e.g. swap files, tmp files).
fn spec_stem_from_path(path: &Path) -> Option<String> {
    let name = path.file_name().and_then(|n| n.to_str())?;
    if let Some(stem) = name.strip_suffix(".json.gz") {
        return Some(stem.to_string());
    }
    if let Some(stem) = name.strip_suffix(".json") {
        return Some(stem.to_string());
    }
    None
}

/// Pipeline result: completion candidates at the cursor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompleteResult {
    /// Ranked candidate list (prefix-filtered).
    pub items: Vec<Suggestion>,
    /// Optional reason when items is empty (debug aid).
    pub reason: Option<String>,
}

/// Run the full pipeline against `line` + `cursor` byte offset.
/// Default match mode = prefix (the v1.0 contract).
pub fn complete(line: &str, cursor: usize, registry: &SpecRegistry) -> CompleteResult {
    complete_in(line, cursor, registry, None, MatchMode::Prefix)
}

/// Same as [`complete`], but uses `cwd` as the working-directory
/// context for filesystem-aware generators (e.g.
/// [`Generator::PackageJsonScripts`]). When `cwd` is `None`, falls
/// back to the daemon process's `current_dir()`.
pub fn complete_in(
    line: &str,
    cursor: usize,
    registry: &SpecRegistry,
    cwd: Option<&std::path::Path>,
    mode: MatchMode,
) -> CompleteResult {
    let cursor = clamp_cursor_to_char_boundary(line, cursor);
    let tokens = tokenize(&line[..cursor]);

    if tokens.is_empty() {
        return CompleteResult {
            items: vec![],
            reason: Some("empty input".into()),
        };
    }

    let prefix = current_prefix(&line[..cursor]);
    let binary = tokens[0].text.as_str();

    let Some(spec) = registry.lookup(binary) else {
        return CompleteResult {
            items: vec![],
            reason: Some(format!("no spec for {binary}")),
        };
    };
    let spec_ref: &Spec = spec.as_ref();

    let result = parse_arguments(spec_ref, &tokens, cursor);
    let current = walk_to_current(spec_ref, &result.subcommand_path).unwrap_or(spec_ref);

    // Surface-form overrides — the state machine doesn't see the
    // partial token (it's after the cursor), so we adjust based on
    // what the user just typed:
    //
    // - prefix starts with `-` → emit options (TS reference behavior)
    // - prefix is empty/word AND current node has subcommands AND no
    //   positional arg consumed yet → emit subcommands. This handles
    //   roots like `git` that have BOTH subcommands AND a fallback
    //   `<alias>` positional — typing `git ` should suggest
    //   subcommands, not the alias arg's (empty) suggestion list.
    // Walk the subcommand chain so emit_options can pull in
    // persistent ancestor options (Fig parity for `--help` /
    // `--debug` declared at the root with isPersistent).
    let chain = walk_chain(spec_ref, &result.subcommand_path);
    let ancestor_refs: Vec<&Subcommand> = chain.iter().rev().skip(1).copied().collect();

    let has_emittable_options = !current.options.is_empty()
        || ancestor_refs
            .iter()
            .any(|sc| sc.options.iter().any(|o| o.is_persistent));
    let prefix_is_option = prefix.starts_with('-') && has_emittable_options;
    let prefer_subcommands = !prefix.starts_with('-')
        && !current.subcommands.is_empty()
        && matches!(
            result.cursor_context,
            CursorContext::Subcommand | CursorContext::Arg
        );

    // Option-arg dispatch: when the parser tells us the cursor is
    // awaiting an option's argument value (e.g. `cargo run --bin
    // <here>`), iterate that option's args[idx] generators directly
    // — `emit_arg_candidates` would otherwise look at the surrounding
    // subcommand's positional args, which is the wrong slot.
    if let Some((opt_name, arg_idx)) = result.active_option_arg.as_ref() {
        // Inherited lookup, not just `current.options`: the parser
        // binds isPersistent ancestor options too (`kubectl get pods
        // -n <here>` with `-n` declared at the root), so the dispatch
        // must search the same scope or those args' generators
        // silently never run and the positional slot wins instead.
        if let Some(opt) =
            crate::spec_parser::find_option_inherited(spec_ref, &result.subcommand_path, opt_name)
        {
            if let Some(arg) = opt.args.get(*arg_idx) {
                let items = emit_candidates_for_arg(arg, &prefix, cwd, Some(opt), mode, &tokens);
                return CompleteResult {
                    items,
                    reason: None,
                };
            }
        }
    }

    let mut items = if prefix_is_option {
        emit_options_with_ancestors(current, &ancestor_refs, &prefix, mode)
    } else if prefer_subcommands {
        // yarn-style shorthand: `yarn web` should match both yarn
        // subcommands (none start with "web") and the root args
        // generator (npmScriptsGenerator → web:start, web:build:dev,
        // …). Merge whenever the level has args with dynamic source.
        let mut subs = emit_subcommands(current, &prefix, mode);
        if arg_has_dynamic_source(current) {
            subs.extend(emit_arg_candidates(current, &prefix, cwd, mode, &tokens));
        }
        // Sort by priority first (script results get priority 75
        // and float above default-50 subcommands), then alpha.
        subs.sort_by(sort_by_priority_then_alpha);
        subs.dedup_by(|a, b| a.display == b.display);
        subs
    } else {
        match result.cursor_context {
            CursorContext::Subcommand => emit_subcommands(current, &prefix, mode),
            CursorContext::OptionName => {
                emit_options_with_ancestors(current, &ancestor_refs, &prefix, mode)
            }
            CursorContext::Arg => emit_arg_candidates(current, &prefix, cwd, mode, &tokens),
            CursorContext::Done => vec![],
        }
    };

    // Drop no-op completions: a suggestion whose insertion is exactly
    // the token already typed adds nothing. The user who typed
    // `git status` in full shouldn't see `status` re-offered — only the
    // "Immediately execute" sentinel (widget-side) plus any longer
    // matches (`status-v2`) remain. Empty prefix means the user is
    // browsing a fresh token (`git `), so keep everything.
    if !prefix.is_empty() {
        items.retain(|s| s.insertion != prefix);
    }

    CompleteResult {
        items,
        reason: None,
    }
}

fn tokenize(text: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut bytes = text.bytes().enumerate().peekable();
    let mut word_start: Option<usize> = None;
    while let Some(&(i, b)) = bytes.peek() {
        if b == b' ' || b == b'\t' {
            if let Some(ws) = word_start.take() {
                out.push(Annotation {
                    span: ws..i,
                    text: text[ws..i].to_string(),
                    kind: TokenKind::Unknown,
                });
            }
            bytes.next();
            start = i + 1;
        } else {
            if word_start.is_none() {
                word_start = Some(i);
            }
            bytes.next();
        }
    }
    if let Some(ws) = word_start {
        out.push(Annotation {
            span: ws..text.len(),
            text: text[ws..].to_string(),
            kind: TokenKind::Unknown,
        });
    }
    let _ = start;
    out
}

/// Trailing partial word for prefix filtering. Empty when the
/// cursor sits after whitespace.
fn current_prefix(text: &str) -> String {
    if text.is_empty() || text.ends_with(' ') || text.ends_with('\t') {
        return String::new();
    }
    let last_ws = text.rfind([' ', '\t']);
    match last_ws {
        Some(i) => text[i + 1..].to_string(),
        None => text.to_string(),
    }
}

/// Sort suggestions by Fig-style priority (higher first, default
/// 50), then alphabetically by display label. Used uniformly by
/// emit_subcommands / emit_options_with_ancestors /
/// emit_candidates_for_arg so the popup order matches Fig's rule:
/// higher priority floats up, ties resolve by name.
fn sort_by_priority_then_alpha(a: &Suggestion, b: &Suggestion) -> std::cmp::Ordering {
    let pa = a.priority.unwrap_or(50);
    let pb = b.priority.unwrap_or(50);
    pb.cmp(&pa).then_with(|| a.display.cmp(&b.display))
}

/// Fig parity filterStrategy matcher.
///
/// Precedence:
/// 1. Spec `filterStrategy: "substring"` always wins (per-arg override).
/// 2. Otherwise user [`MatchMode`] applies — `Fuzzy` enables case-insensitive
///    subsequence matching; `Prefix` is the v1.0 default.
fn matches_filter(name: &str, query: &str, strategy: Option<&str>, mode: MatchMode) -> bool {
    if let Some("substring") = strategy {
        return name.contains(query);
    }
    match mode {
        MatchMode::Fuzzy => fuzzy_subsequence_match(name, query),
        MatchMode::Prefix => name.starts_with(query),
    }
}

/// Mode-aware name gate for subcommand / option / generator outputs
/// that don't carry a `filterStrategy` of their own.
fn matches_name(name: &str, prefix: &str, mode: MatchMode) -> bool {
    match mode {
        MatchMode::Fuzzy => fuzzy_subsequence_match(name, prefix),
        MatchMode::Prefix => name.starts_with(prefix),
    }
}

/// Case-insensitive subsequence match — every char of `query` appears
/// in `name` in order, with arbitrary gaps. Empty query matches
/// everything (the `git ⎵` case stays valid).
fn fuzzy_subsequence_match(name: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut q = query.chars();
    let mut next = q.next();
    for nc in name.chars() {
        if let Some(qc) = next {
            if nc.eq_ignore_ascii_case(&qc) {
                next = q.next();
                if next.is_none() {
                    return true;
                }
            }
        }
    }
    next.is_none()
}

/// Fig parity getQueryTerm splitter. Given the raw typed token and
/// a string of delimiter chars (`","`, `"@"`, …), return
/// `(query, insert_prefix)` where `query` is the substring AFTER
/// the last delimiter (or whole token when no delim found), and
/// `insert_prefix` is the part to preserve in the insertion so
/// pressing Tab doesn't clobber the already-typed leading text.
fn split_by_query_term<'a>(prefix: &'a str, delims: Option<&str>) -> (&'a str, &'a str) {
    let Some(delims) = delims else {
        return (prefix, "");
    };
    if delims.is_empty() {
        return (prefix, "");
    }
    // Find last byte position where any delimiter char occurs.
    let mut last: Option<usize> = None;
    for (i, ch) in prefix.char_indices() {
        if delims.chars().any(|d| d == ch) {
            last = Some(i + ch.len_utf8());
        }
    }
    match last {
        Some(idx) => (&prefix[idx..], &prefix[..idx]),
        None => (prefix, ""),
    }
}

/// Fig parity icon sanitizer. Many specs reference Fig's icon
/// registry via `fig://icon?type=...` URLs which mean nothing in a
/// terminal. Strip those and accept only short visible glyphs
/// (typically a single emoji or one ASCII char). Anything longer
/// than 4 bytes is also rejected as defense against accidental
/// long-string injection that would distort popup row widths.
///
/// Width contract: the widget reserves exactly **2 cells** for any
/// non-ASCII glyph (so all rows align). To uphold that, non-ASCII
/// glyphs whose terminal-display width is not 2 (Latin-extended like
/// `à`, ambiguous-width like `⚠` without VS-16, zero-width marks,
/// etc.) are rejected too — they would render in 1 cell and shift
/// the row by 1. ASCII single chars use the 1-cell slot.
fn sanitize_icon(raw: Option<&str>) -> Option<String> {
    use unicode_width::UnicodeWidthStr;
    let s = raw?.trim();
    if s.is_empty() || s.starts_with("fig://") || s.len() > 4 {
        return None;
    }
    let w = UnicodeWidthStr::width(s);
    if s.is_ascii() {
        if w == 1 { Some(s.to_string()) } else { None }
    } else if w == 2 {
        Some(s.to_string())
    } else {
        None
    }
}

fn walk_to_current<'a>(root: &'a Spec, path: &[String]) -> Option<&'a Subcommand> {
    let mut node = root;
    for name in path.iter().skip(1) {
        node = find_subcommand(node, name)?;
    }
    Some(node)
}

/// Like [`walk_to_current`] but returns the whole chain root → leaf.
/// Used to enumerate ancestor subcommands when looking up
/// persistent options. Returns at least `[root]` for an empty
/// path.
fn walk_chain<'a>(root: &'a Spec, path: &[String]) -> Vec<&'a Subcommand> {
    let mut chain: Vec<&Subcommand> = vec![root];
    let mut node: &Subcommand = root;
    for name in path.iter().skip(1) {
        match find_subcommand(node, name) {
            Some(next) => {
                node = next;
                chain.push(next);
            }
            None => break,
        }
    }
    chain
}

/// Fig-style positional-argument hint for a subcommand, e.g.
/// `[remote] [branch]` (git push) or `<file>`. Optional args are
/// bracketed `[name]`, required args angle-bracketed `<name>`, variadic
/// args get a trailing `...`. Args with no name carry nothing to show
/// and are skipped. Returns "" when there is no named positional arg.
///
/// The hint is appended to the popup `display` only — never to
/// `insertion` (accepting `push` must not type the template) nor to the
/// ghost (which mirrors insertion). The widget renders the trailing
/// hint dimmer than the command name.
fn arg_hint(args: &[crate::spec_parser::Arg]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for a in args {
        let Some(name) = a.name.as_deref() else { continue };
        if name.is_empty() {
            continue;
        }
        let ellipsis = if a.is_variadic { "..." } else { "" };
        parts.push(if a.is_optional {
            format!("[{name}{ellipsis}]")
        } else {
            format!("<{name}{ellipsis}>")
        });
    }
    parts.join(" ")
}

fn emit_subcommands(node: &Subcommand, prefix: &str, mode: MatchMode) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = node
        .subcommands
        .iter()
        .filter(|sc| !sc.hidden)
        .filter(|sc| name_or_aliases_match(&sc.name, &sc.aliases, prefix, mode))
        .map(|sc| {
            let hint = arg_hint(&sc.args);
            let display = if hint.is_empty() {
                sc.name.clone()
            } else {
                format!("{} {hint}", sc.name)
            };
            Suggestion {
                insertion: sc.name.clone(),
                display,
                description: sc.description.clone(),
                kind: SuggestionKind::Subcommand,
                priority: sc.priority,
                icon: sanitize_icon(sc.icon.as_deref()),
            }
        })
        .collect();
    out.sort_by(sort_by_priority_then_alpha);
    out
}

/// Emit option-name suggestions for `node`, including any
/// ancestor options whose `is_persistent` flag is set (Fig parity).
/// `ancestors` is leaf → root order of the chain ABOVE `node`;
/// pass an empty slice for a root-level emit.
fn emit_options_with_ancestors(
    node: &Subcommand,
    ancestors: &[&Subcommand],
    prefix: &str,
    mode: MatchMode,
) -> Vec<Suggestion> {
    let emit = |opt: &crate::spec_parser::Opt| -> Vec<Suggestion> {
        // Fig parity: when `requiresSeparator` is set and the option
        // takes args, append `=` to the insertion so the cursor
        // continues into the arg in one keystroke (`--color=`).
        let needs_eq = opt.requires_separator && !opt.args.is_empty();
        opt.names
            .iter()
            .filter(|n| matches_name(n, prefix, mode))
            .map(|n| Suggestion {
                insertion: if needs_eq { format!("{n}=") } else { n.clone() },
                display: n.clone(),
                description: opt.description.clone(),
                kind: SuggestionKind::Flag,
                priority: opt.priority,
                icon: sanitize_icon(opt.icon.as_deref()),
            })
            .collect()
    };
    let mut out: Vec<Suggestion> = node
        .options
        .iter()
        .filter(|o| !o.hidden)
        .flat_map(&emit)
        .collect();
    for sc in ancestors {
        for opt in &sc.options {
            if !opt.is_persistent || opt.hidden {
                continue;
            }
            // Don't double-emit if leaf already declared the same flag.
            if opt
                .names
                .iter()
                .any(|n| node.options.iter().any(|local| local.names.contains(n)))
            {
                continue;
            }
            out.extend(emit(opt));
        }
    }
    out.sort_by(sort_by_priority_then_alpha);
    out.dedup_by(|a, b| a.display == b.display);
    out
}

/// `true` when the node's first positional arg carries either
/// suggestions or a generator we know how to drive. Used to decide
/// whether to merge arg candidates into a subcommand-context emit
/// (yarn-shorthand pattern: `yarn web<Tab>` → npm scripts).
fn arg_has_dynamic_source(node: &Subcommand) -> bool {
    let Some(arg) = node.args.first() else {
        return false;
    };
    if !arg.suggestions.is_empty() {
        return true;
    }
    arg.generators.iter().any(|g| {
        matches!(
            g,
            crate::spec_parser::Generator::Template { .. }
                | crate::spec_parser::Generator::PackageJsonScripts
        )
    })
}

fn emit_arg_candidates(
    node: &Subcommand,
    prefix: &str,
    cwd: Option<&std::path::Path>,
    mode: MatchMode,
    tokens: &[Annotation],
) -> Vec<Suggestion> {
    let Some(arg) = node.args.first() else {
        return vec![];
    };
    emit_candidates_for_arg(arg, prefix, cwd, None, mode, tokens)
}

/// `enclosing_opt` lets the caller pass the OPTION wrapping the arg
/// when dispatching for `cargo run --bin <here>`-style option args.
/// Two uses: (a) description-extraction fallback sees the option's
/// `{a|b|c}` hints, and (b) name-based filepaths inference falls
/// back to the option flag name (`--file`, `-o`) when the arg name
/// itself is uninformative (`name: "string"`).
fn emit_candidates_for_arg(
    arg: &crate::spec_parser::Arg,
    prefix: &str,
    cwd: Option<&std::path::Path>,
    enclosing_opt: Option<&crate::spec_parser::Opt>,
    mode: MatchMode,
    tokens: &[Annotation],
) -> Vec<Suggestion> {
    let enclosing_description = enclosing_opt.and_then(|o| o.description.as_deref());
    // Fig parity getQueryTerm: when the arg declares delimiter chars
    // (e.g. ',' for `cargo search "tokio,serde"`), split the typed
    // token into context-prefix + query-prefix. Matches operate on
    // the query part; the insertion preserves the context prefix so
    // the existing typed text isn't clobbered on Tab.
    let (query, insert_prefix) = split_by_query_term(prefix, arg.get_query_term.as_deref());
    let strategy = arg.filter_strategy.as_deref();
    // Description-extracted enum suggestions (Tier A-ish). Many Fig
    // specs put `{a|b|c}` directly in the description text instead
    // of populating `suggestions[]` — recover those as candidates
    // when the spec didn't otherwise provide any. Conservative
    // pattern: braces + ≥2 pipe-separated alphanumeric tokens.
    let desc_for_extract = arg.description.as_deref().or(enclosing_description);
    let mut out: Vec<Suggestion> = desc_for_extract
        .and_then(extract_enum_from_description)
        .into_iter()
        .flatten()
        .filter(|s| matches_filter(s, query, strategy, mode))
        .map(|s| Suggestion {
            insertion: format!("{insert_prefix}{s}"),
            display: s,
            description: desc_for_extract.map(|d| d.to_string()),
            kind: SuggestionKind::Argument,
            priority: None,
            icon: None,
        })
        .collect();

    // Static suggestions list (Tier A). Each entry may be a bare
    // string or a rich {name, description, displayName, insertValue,
    // icon, priority} object — we surface description / displayName
    // / insertValue here for Fig parity, fallback to .name for the
    // others.
    out.extend(
        arg.suggestions
            .iter()
            .filter(|s| matches_filter(&s.name, query, strategy, mode))
            .map(|s| {
                let base = s.insert_value.clone().unwrap_or_else(|| s.name.clone());
                Suggestion {
                    insertion: format!("{insert_prefix}{base}"),
                    display: s.display_name.clone().unwrap_or_else(|| s.name.clone()),
                    description: s.description.clone(),
                    kind: SuggestionKind::Argument,
                    priority: s.priority,
                    icon: sanitize_icon(s.icon.as_deref()),
                }
            }),
    );

    // `template:` field on the arg (separate from `generators:`).
    // Covers `cat`, `vim`, `ls`, `man`, etc. — most unix command
    // specs declare `template: filepaths` or `template: folders`
    // instead of a generator. The filesystem walker is the same
    // path used by Generator::Filepaths. Help templates have no
    // native handler yet.
    if let Some(folders_only) = match arg.template {
        Some(crate::spec_parser::TemplateKind::Folders) => Some(true),
        Some(crate::spec_parser::TemplateKind::Filepaths) => Some(false),
        _ => None,
    } {
        if let Some(paths) = filepaths_at(cwd, prefix, folders_only) {
            out.extend(
                paths
                    .into_iter()
                    .map(|(insertion, display, description, icon)| Suggestion {
                        insertion,
                        display,
                        description,
                        kind: SuggestionKind::Argument,
                        priority: None,
                        icon,
                    }),
            );
        }
    }
    if matches!(
        arg.template,
        Some(crate::spec_parser::TemplateKind::History)
    ) {
        if let Some(entries) = shell_history_entries() {
            out.extend(
                entries
                    .into_iter()
                    .filter(|s| matches_name(s, prefix, mode))
                    .map(|s| Suggestion {
                        insertion: s.clone(),
                        display: s,
                        description: Some("history".into()),
                        kind: SuggestionKind::Argument,
                        priority: None,
                        icon: None,
                    }),
            );
        }
    }

    // Tier B: spawn `Generator::Template` scripts and parse stdout
    // lines as candidates. Skipped under NERV_NO_GENERATORS=1 (tests,
    // sandboxed environments).
    if std::env::var_os("NERV_NO_GENERATORS").is_none() {
        for g in &arg.generators {
            match g {
                crate::spec_parser::Generator::Template { script } => {
                    if let Some(lines) = cached_template_generator(script, cwd) {
                        out.extend(
                            lines
                                .into_iter()
                                .map(|line| split_id_label(&line))
                                .filter(|(ins, _)| matches_name(ins, prefix, mode))
                                .map(|(insertion, display)| Suggestion {
                                    insertion,
                                    display,
                                    description: None,
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::PackageJsonScripts => {
                    if let Some(scripts) = package_json_scripts(cwd) {
                        out.extend(
                            scripts
                                .into_iter()
                                .filter(|(name, _)| matches_name(name, prefix, mode))
                                .map(|(name, cmd)| {
                                    // `!`-prefixed scripts are widely
                                    // used as visual headers/dividers
                                    // (e.g. `"!cli": "─── CLI scripts
                                    // ──"`). They're not meant to run.
                                    // Push them below subcommands so
                                    // real scripts surface first; keep
                                    // them visible at the bottom.
                                    let is_header = name.starts_with('!');
                                    Suggestion {
                                        insertion: name.clone(),
                                        display: name,
                                        description: Some(cmd),
                                        kind: SuggestionKind::Argument,
                                        priority: Some(if is_header { 25 } else { 75 }),
                                        // Q-style glyph: `$` colored
                                        // purple by the widget's ICON
                                        // escape. Visually distinct
                                        // from folder (📁) and bare
                                        // subcommand rows (blank).
                                        icon: Some("$".into()),
                                    }
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::Filepaths { folders_only } => {
                    if let Some(paths) = filepaths_at(cwd, prefix, *folders_only) {
                        out.extend(paths.into_iter().map(
                            |(insertion, display, description, icon)| Suggestion {
                                insertion,
                                display,
                                description,
                                kind: SuggestionKind::Argument,
                                priority: None,
                                icon,
                            },
                        ));
                    }
                }
                crate::spec_parser::Generator::SshHosts => {
                    if let Some(hosts) = ssh_hosts() {
                        out.extend(
                            hosts
                                .into_iter()
                                .filter(|h| matches_name(h, prefix, mode))
                                .map(|h| Suggestion {
                                    insertion: h.clone(),
                                    display: h,
                                    description: Some("SSH host".into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::MakefileTargets => {
                    if let Some(targets) = makefile_targets(cwd) {
                        out.extend(
                            targets
                                .into_iter()
                                .filter(|t| matches_name(t, prefix, mode))
                                .map(|t| Suggestion {
                                    insertion: t.clone(),
                                    display: t,
                                    description: Some("make target".into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::ManPages => {
                    if let Some(pages) = man_pages() {
                        out.extend(
                            pages
                                .into_iter()
                                .filter(|p| matches_name(p, prefix, mode))
                                .map(|p| Suggestion {
                                    insertion: p.clone(),
                                    display: p,
                                    description: Some("man page".into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::PackageJsonDeps => {
                    if let Some(deps) = package_json_deps(cwd) {
                        out.extend(
                            deps.into_iter()
                                .filter(|(name, _)| matches_name(name, prefix, mode))
                                .map(|(name, kind)| Suggestion {
                                    insertion: name.clone(),
                                    display: name,
                                    description: Some(kind.into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::KubectlResources => {
                    let key = vec![
                        "kubectl".to_string(),
                        "api-resources".to_string(),
                        "-o".to_string(),
                        "name".to_string(),
                    ];
                    // Cluster-global (~/.kube/config) — cwd-independent.
                    if let Some(lines) = cached_template_generator(&key, None) {
                        out.extend(
                            lines
                                .into_iter()
                                .filter(|s| matches_name(s, prefix, mode))
                                .map(|line| Suggestion {
                                    insertion: line.clone(),
                                    display: line,
                                    description: Some("k8s resource".into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::CargoTargets { kind } => {
                    if let Some(targets) = cargo_targets(cwd, kind.as_deref()) {
                        out.extend(
                            targets
                                .into_iter()
                                .filter(|(name, _, _)| matches_name(name, prefix, mode))
                                .map(|(name, kind, path)| Suggestion {
                                    insertion: name.clone(),
                                    display: name,
                                    description: Some(if path.is_empty() {
                                        kind
                                    } else {
                                        format!("{kind} — {path}")
                                    }),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::AwsList {
                    service,
                    verb,
                    lookup_flags,
                    parent_key,
                    id_field,
                } => {
                    let cmd = build_aws_list_command(service, verb, lookup_flags, tokens);
                    // AWS CLI reads ~/.aws, not cwd — keep the cache global.
                    if let Some(lines) = cached_template_generator(&cmd, None) {
                        let stdout = lines.join("\n");
                        if let Some(names) =
                            extract_aws_json_names(&stdout, parent_key, id_field.as_deref())
                        {
                            out.extend(
                                names
                                    .into_iter()
                                    .filter(|s| matches_name(s, prefix, mode))
                                    .map(|name| Suggestion {
                                        insertion: name.clone(),
                                        display: name,
                                        description: Some("aws".into()),
                                        kind: SuggestionKind::Argument,
                                        priority: None,
                                        icon: None,
                                    }),
                            );
                        }
                    }
                }
                crate::spec_parser::Generator::ScriptWithJsonPath {
                    script,
                    parent_key,
                    id_field,
                } => {
                    // Feed the *raw* stdout to the explicit json-path
                    // extractor — `cached_template_generator` would have
                    // already mangled `{`/`[`-leading output via
                    // `extract_json_candidates`, leaving nothing for
                    // `parent_key`/`id_field` to navigate. AWS CLI reads
                    // ~/.aws, not cwd, so the raw cache stays global too.
                    if let Some(raw) = cached_script_raw(script) {
                        if let Some(names) =
                            extract_aws_json_names(&raw, parent_key, id_field.as_deref())
                        {
                            out.extend(
                                names
                                    .into_iter()
                                    .filter(|s| matches_name(s, prefix, mode))
                                    .map(|name| Suggestion {
                                        insertion: name.clone(),
                                        display: name,
                                        description: None,
                                        kind: SuggestionKind::Argument,
                                        priority: None,
                                        icon: None,
                                    }),
                            );
                        }
                    }
                }
                #[cfg(feature = "quickjs")]
                crate::spec_parser::Generator::Custom {
                    source: Some(source),
                    ..
                } => {
                    // Tier C: spin up a fresh QuickJS sandbox per call,
                    // run the captured closure with the live token list,
                    // and surface returned strings. Soft-fail on any
                    // error (parse / throw / timeout / non-array) —
                    // the dispatcher just falls through to the next
                    // generator or the smart fallback.
                    let token_strs: Vec<String> = tokens.iter().map(|a| a.text.clone()).collect();
                    if let Some(cands) = crate::tier_c::execute_custom_source(source, &token_strs) {
                        out.extend(
                            cands
                                .into_iter()
                                .filter(|s| matches_name(s, prefix, mode))
                                .map(|s| Suggestion {
                                    insertion: s.clone(),
                                    display: s,
                                    description: None,
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                    icon: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::ZoxideQuery => {
                    if let Some(rows) = zoxide_query() {
                        // z / zoxide are fuzzy by design — `z claud`
                        // should match `~/.claude` even though the
                        // folder name is `.claude`. rank_zoxide_matches
                        // keeps name hits ahead of path-only hits and
                        // frecency order within each; encode the rank as
                        // a descending priority so the emit-wide
                        // sort_by_priority_then_alpha preserves it
                        // instead of re-alphabetising (which buried the
                        // literal `encl` match under `app`/`apps`).
                        for (rank, (name, path, score)) in
                            rank_zoxide_matches(rows, prefix).into_iter().enumerate()
                        {
                            out.push(Suggestion {
                                insertion: name.clone(),
                                display: name,
                                description: Some(format!("{path} (score {score:.1})")),
                                kind: SuggestionKind::Argument,
                                priority: Some(10_000u32.saturating_sub(rank as u32)),
                                icon: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Smart fallback: when the arg has no template / no generators /
    // no suggestions but the arg name semantically implies a path,
    // treat it as a filepaths/folders walk. Covers docker build
    // (arg.name = "path"), `find <path>`, and dozens of similar
    // unix-style specs where the spec author forgot the template
    // hint. Falls back to the enclosing option's flag name when the
    // arg name is uninformative (`docker build -f` has arg.name =
    // "string" but the option is `-f / --file`).
    // Only kicks in when nothing else fired.
    if out.is_empty() && std::env::var_os("NERV_NO_GENERATORS").is_none() {
        let kind = infer_filepaths_kind(arg.name.as_deref())
            .or_else(|| infer_filepaths_kind_from_opt_names(enclosing_opt));
        if let Some(folders_only) = kind {
            if let Some(paths) = filepaths_at(cwd, prefix, folders_only) {
                out.extend(
                    paths
                        .into_iter()
                        .map(|(insertion, display, description, icon)| Suggestion {
                            insertion,
                            display,
                            description,
                            kind: SuggestionKind::Argument,
                            priority: None,
                            icon,
                        }),
                );
            }
        }
    }

    out.sort_by(sort_by_priority_then_alpha);
    out.dedup_by(|a, b| a.display == b.display);
    out
}

/// Infer whether an arg with no explicit template / generators
/// should be treated as a path. Returns `Some(folders_only)` to
/// activate filepaths walk, or `None` to leave the arg empty.
///
/// Conservative — only matches exact lowercase single-word names
/// to avoid hijacking args like "filename for output" that mean
/// something more specific than a generic path picker.
fn infer_filepaths_kind(name: Option<&str>) -> Option<bool> {
    let n = name?.trim().to_ascii_lowercase();
    match n.as_str() {
        "path" | "file" | "files" | "filepath" | "filename" | "src" | "dest" | "source"
        | "destination" | "input" | "output" => Some(false),
        "dir" | "directory" | "folder" | "dirname" | "dirpath" => Some(true),
        _ => None,
    }
}

/// Same idea as [`infer_filepaths_kind`] but consults the wrapping
/// option's flag names. Recovers args whose own `name` is generic
/// ("string") but whose option is unmistakably a path:
/// `-f / --file`, `-o / --output`, `-d / --directory`. Long names
/// take precedence — short flags (`-d` could be delete OR
/// directory) only count when no long form is present.
fn infer_filepaths_kind_from_opt_names(opt: Option<&crate::spec_parser::Opt>) -> Option<bool> {
    let opt = opt?;
    let names: Vec<String> = opt.names.iter().map(|n| n.to_ascii_lowercase()).collect();
    for n in &names {
        if let Some(long) = n.strip_prefix("--") {
            match long {
                "file" | "files" | "filename" | "filepath" | "input" | "output" | "log"
                | "log-file" | "input-file" | "output-file" => return Some(false),
                "dir" | "directory" | "folder" | "input-dir" | "output-dir" | "workdir"
                | "working-dir" | "chdir" => return Some(true),
                _ => {}
            }
        }
    }
    // Short flag fallback — only when no long-name disambiguation
    // exists. `-d` is ambiguous (delete vs directory) so we don't
    // accept it here; require an explicit long name.
    for n in &names {
        if n == "-f" {
            return Some(false);
        }
    }
    None
}

type GeneratorCacheKey = (Vec<String>, Option<PathBuf>);
type GeneratorCacheMap = HashMap<GeneratorCacheKey, (std::time::Instant, Vec<String>)>;

/// Process-wide cache for Tier B generator results.
/// Key: `(script argv, spawn cwd)`. The cwd is part of the key because
/// cwd-sensitive generators (`git branch -a`, …) produce different
/// output per directory — keying on argv alone leaked one repo's
/// branches into another (Fig #2101 / #2026 / #2268). Value:
/// (insertion-time, captured stdout lines). TTL: 5s. Max entries: 64
/// (oldest-evicted on overflow). Keeps per-keystroke completion calls
/// from re-spawning the same shell command (e.g. `git branch --list`).
static GENERATOR_CACHE: std::sync::LazyLock<std::sync::Mutex<GeneratorCacheMap>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

const GENERATOR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const GENERATOR_CACHE_MAX: usize = 64;

type RawScriptCacheMap = HashMap<Vec<String>, (std::time::Instant, String)>;

/// Process-wide cache for `ScriptWithJsonPath` raw stdout, keyed by script
/// argv. Same TTL / size / eviction policy as [`GENERATOR_CACHE`]; separate
/// because the value is the verbatim blob (not post-processed lines).
static SCRIPT_RAW_CACHE: std::sync::LazyLock<std::sync::Mutex<RawScriptCacheMap>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Cached wrapper around [`execute_template_generator`]. Returns
/// the cached value on TTL-fresh hit, otherwise runs the generator
/// and inserts the result.
fn cached_template_generator(script: &[String], cwd: Option<&Path>) -> Option<Vec<String>> {
    let key: GeneratorCacheKey = (script.to_vec(), cwd.map(Path::to_path_buf));
    if let Ok(cache) = GENERATOR_CACHE.lock() {
        if let Some((stamp, lines)) = cache.get(&key) {
            if stamp.elapsed() < GENERATOR_CACHE_TTL {
                return Some(lines.clone());
            }
        }
    }
    let lines = execute_template_generator(script, cwd)?;
    if let Ok(mut cache) = GENERATOR_CACHE.lock() {
        if cache.len() >= GENERATOR_CACHE_MAX {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (std::time::Instant::now(), lines.clone()));
    }
    Some(lines)
}

/// Run a Tier B template generator script and return its stdout lines.
///
/// Hard-capped at 800ms wall time. The engine itself has a 25ms p95
/// budget for cached lookups, but the first call to any generator
/// inevitably costs whatever the underlying shell command takes.
/// 200ms was too tight for `brew list -1` (~450ms cold on a busy
/// machine) and `gh repo list` (network); 800ms covers those without
/// pushing UX into noticeably-laggy territory, and the 5s in-memory
/// cache means every keystroke after the first hits cache anyway.
const GENERATOR_TIMEOUT_MS: u64 = 800;

/// Drain a spawned child process's stdout into a single Vec under
/// [`GENERATOR_TIMEOUT_MS`]. On timeout, kill the child and return
/// `None`. Otherwise return the captured bytes (which may be empty
/// when the child wrote nothing).
///
/// A dedicated thread does the read so the pipe buffer (~64 KB on
/// macOS) never fills and blocks the child. An earlier `try_wait()` +
/// 10ms tick loop without reading the pipe deadlocked on commands
/// that wrote more than ~64 KB before exiting (e.g. `ps axo
/// pid,comm` on a busy machine: 1600+ lines / ~50 KB) — even when
/// the command itself finished in <100 ms.
///
/// `buf_cap` sets the initial Vec capacity; pick the rough expected
/// payload size to avoid reallocs (8 KB for line-shaped Fig
/// generators, 64 KB for blob payloads like `cargo metadata`).
fn spawn_with_timeout(mut child: std::process::Child, buf_cap: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(buf_cap);
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(Duration::from_millis(GENERATOR_TIMEOUT_MS)) {
        Ok(buf) => {
            let _ = child.wait();
            Some(buf)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Cap unbounded `git log` / `git rev-list` history walks so they stay
/// within the generator timeout on large repositories (Fig #2607).
/// `git rev-list --all --oneline` enumerates *every* commit, which on a
/// big repo runs past `GENERATOR_TIMEOUT_MS` and the child is killed →
/// zero candidates. Completion only ever shows a prefix-filtered handful,
/// so the 1000 most-recent commits are plenty. Left untouched when the
/// caller already bounds the walk, when a `--` pathspec separator is
/// present (appending would be read as a path), or for any non-history
/// git command.
fn cap_git_history(script: &[String]) -> Vec<String> {
    let is_git = script.first().map(|b| b == "git").unwrap_or(false);
    let walks_history = script.iter().any(|a| a == "log" || a == "rev-list");
    let has_pathspec_sep = script.iter().any(|a| a == "--");
    if !is_git || !walks_history || has_pathspec_sep {
        return script.to_vec();
    }
    let already_bounded = script.iter().any(|a| {
        a == "-n"
            || a == "--max-count"
            || a.starts_with("--max-count=")
            || (a.len() > 1 && a.starts_with('-') && a[1..].chars().all(|c| c.is_ascii_digit()))
    });
    if already_bounded {
        return script.to_vec();
    }
    let mut capped = script.to_vec();
    capped.push("--max-count=1000".to_string());
    capped
}

fn execute_template_generator(script: &[String], cwd: Option<&Path>) -> Option<Vec<String>> {
    use std::process::{Command, Stdio};
    if script.is_empty() {
        return None;
    }
    let capped = cap_git_history(script);
    let bin = capped.first()?;
    let args = &capped[1..];
    let mut command = Command::new(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Spawn in the client shell's cwd so cwd-sensitive generators (git
    // branch/log/remote, …) reflect the user's repo, not nervd's process
    // directory (§4 IPC cwd invariant). None → inherit the daemon cwd.
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let child = command.spawn().ok()?;
    let buf = spawn_with_timeout(child, 8192)?;
    // Don't gate on exit status alone — many Fig generators run
    // `find $i ...` over `$PATH`-derived dirs that may not all exist,
    // so the script exits non-zero on the missing-path case even
    // though stdout was usefully populated. If we got stdout bytes,
    // use them; only return None when there's truly nothing to parse.
    if buf.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    // JSON-shaped payloads (`gh repo list --json=...`, `kubectl get -o
    // json`, etc.) need a different reader. Try to extract one
    // candidate per array element; bail to None when we can't make
    // sense of the structure.
    if let Some(first) = text.trim_start().chars().next() {
        if first == '[' || first == '{' {
            return extract_json_candidates(&text);
        }
    }
    let lines: Vec<String> = text
        .lines()
        .map(sanitize_generator_line)
        .filter(|s| !s.is_empty())
        .collect();
    Some(lines)
}

/// Run a generator script and return its **raw** stdout, cached for
/// [`GENERATOR_CACHE_TTL`]. Unlike [`cached_template_generator`] this does no
/// post-processing: `Generator::ScriptWithJsonPath` needs the verbatim blob
/// so its explicit `parent_key`/`id_field` navigation runs. The template path
/// intercepts any `{`/`[`-leading output with `extract_json_candidates`,
/// which guesses at the array + label field — for a nested payload like
/// `cargo metadata` (`{packages:[…], workspace_members:[…], resolve:…}`) it
/// picks the wrong array and yields nothing, so the json-path generator must
/// bypass it. 64 KB initial capacity (cargo metadata clears 70 KB on real
/// workspaces); `read_to_end` grows past it regardless.
fn cached_script_raw(script: &[String]) -> Option<String> {
    use std::process::{Command, Stdio};
    if script.is_empty() {
        return None;
    }
    let key = script.to_vec();
    if let Ok(cache) = SCRIPT_RAW_CACHE.lock() {
        if let Some((stamp, blob)) = cache.get(&key) {
            if stamp.elapsed() < GENERATOR_CACHE_TTL {
                return Some(blob.clone());
            }
        }
    }
    let bin = script.first()?;
    let child = Command::new(bin)
        .args(&script[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let buf = spawn_with_timeout(child, 65_536)?;
    if buf.is_empty() {
        return None;
    }
    let blob = String::from_utf8_lossy(&buf).into_owned();
    if let Ok(mut cache) = SCRIPT_RAW_CACHE.lock() {
        if cache.len() >= GENERATOR_CACHE_MAX {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (std::time::Instant::now(), blob.clone()));
    }
    Some(blob)
}

/// Split a multi-column Tier B output line into `(insertion, display)`.
/// When the first whitespace-separated token looks like an id — a
/// numeric pid (`1234 /bin/zsh` from `ps axo pid,comm`) or a git commit
/// hash (`abc1234 fix(cli): …` from `git log --oneline`, Fig #2606) —
/// the bare id becomes the insertion and the original line stays as the
/// display label. Otherwise the full line is used as both — covers
/// single-column generators (`brew list -1`, `kubectl -o name`, branch
/// lists, etc.) where the label IS the insertion.
fn split_id_label(raw: &str) -> (String, String) {
    let trimmed = raw.trim_start();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim_start();
    let id_like = !first.is_empty()
        && !rest.is_empty()
        && (first.chars().all(|c| c.is_ascii_digit()) || is_commit_hash(first));
    if id_like {
        (first.to_string(), trimmed.to_string())
    } else {
        (raw.to_string(), raw.to_string())
    }
}

/// A git short/long commit hash as emitted by `git log --oneline`:
/// 7–40 lowercase hex chars. Used by [`split_id_label`] to make the bare
/// hash the insertion while the `<hash> <subject>` line stays the display
/// (Fig #2606). Lowercase-only to avoid matching ALL-CAPS English words.
fn is_commit_hash(tok: &str) -> bool {
    (7..=40).contains(&tok.len())
        && tok
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Convert a JSON payload from a Tier B generator (gh `--json=…`,
/// kubectl `-o json`, etc.) into a flat list of suggestion strings.
/// Strategy: look for an array (top-level or nested under a likely
/// key like `items` / `data`); for each object element pick the first
/// "label-shaped" string field (name → number → id → title → key); for
/// each scalar element take its string form. Returns `None` when the
/// payload doesn't fit any of those shapes — caller falls back to
/// empty suggestions, which beats dumping JSON noise.
fn extract_json_candidates(raw: &str) -> Option<Vec<String>> {
    // JSON-lines first: `docker ps --format '{{ json . }}'`,
    // `gh repo list --json …` (when piped). One object per line,
    // each starts with `{`. Single `serde_json::from_str` over the
    // whole payload fails because the chunks are concatenated, not
    // wrapped in `[...]`. Detect by checking that >50% of non-blank
    // lines start with `{` and that the first line parses.
    let trimmed = raw.trim();
    let mut maybe_jsonl = false;
    if trimmed.starts_with('{') {
        let lines: Vec<&str> = trimmed
            .lines()
            .map(str::trim_start)
            .filter(|s| !s.is_empty())
            .collect();
        // Accept any non-empty stream where every non-blank line is
        // an object — covers `docker ps --format '{{ json . }}'`
        // (which emits ONE line per container, often a single line).
        if !lines.is_empty() && lines.iter().all(|l| l.starts_with('{')) {
            maybe_jsonl = true;
        }
    }
    if maybe_jsonl {
        let labels: Vec<String> = trimmed
            .lines()
            .filter_map(|line| {
                let s = line.trim();
                if s.is_empty() {
                    return None;
                }
                let v: serde_json::Value = serde_json::from_str(s).ok()?;
                label_from_value(&v)
            })
            .collect();
        if !labels.is_empty() {
            return Some(labels);
        }
    }
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let arr = locate_array(&v)?;
    let labels: Vec<String> = arr.iter().filter_map(label_from_value).collect();
    if labels.is_empty() {
        return None;
    }
    Some(labels)
}

/// Find the most-likely-relevant JSON array. Top-level array wins;
/// otherwise the first array nested under `items` / `data` / `results`
/// (covers kubectl `-o json` → `{"items":[…]}` and similar wrappers).
fn locate_array(v: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    if let serde_json::Value::Array(a) = v {
        return Some(a);
    }
    if let serde_json::Value::Object(obj) = v {
        for key in ["items", "data", "results"] {
            if let Some(serde_json::Value::Array(a)) = obj.get(key) {
                return Some(a);
            }
        }
    }
    None
}

/// Pick a single label string from one array element. Preferred
/// object-key order matches Fig spec postProcess conventions:
/// `name` (most generators), `number` (gh pr/issue), `id`, `title`,
/// `metadata.name` (kubernetes), `key`.
fn label_from_value(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Object(obj) => {
            // Case-insensitive key match — docker emits `Names`,
            // kubectl emits `metadata.name`, gh emits `name`/`number`.
            // Build a lower-cased view so the same generator handles
            // all three without per-tool special-casing.
            let lower_keys: std::collections::HashMap<String, &serde_json::Value> = obj
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect();
            for key in [
                "name",
                "names",
                "unit",
                "unit_file",
                "number",
                "id",
                "title",
                "key",
            ] {
                if let Some(found) = lower_keys.get(key) {
                    if let Some(s) = scalar_to_string(found) {
                        return Some(s);
                    }
                }
            }
            // Kubernetes-style nested metadata.name fallback.
            if let Some(serde_json::Value::Object(meta)) = obj.get("metadata") {
                if let Some(found) = meta.get("name") {
                    if let Some(s) = scalar_to_string(found) {
                        return Some(s);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn scalar_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Mirror of `postPrecessGenerator(out, parentKey, idField)` from the
/// vendor aws specs: parse `stdout` as JSON, descend to `parent_key`,
/// then map each array element to either `element[id_field]` (when
/// `id_field` is `Some`) or the element itself (scalar).
///
/// When the value at `parent_key` is NOT an array but a single object,
/// the upstream closure emits a single suggestion from `elm[childKey]`.
/// Preserved here for parity.
fn extract_aws_json_names(
    stdout: &str,
    parent_key: &str,
    id_field: Option<&str>,
) -> Option<Vec<String>> {
    let root: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let target = root.get(parent_key)?;
    match target {
        serde_json::Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|elm| match id_field {
                    Some(key) => elm.get(key).and_then(scalar_to_string),
                    None => scalar_to_string(elm),
                })
                .collect(),
        ),
        single => id_field
            .and_then(|key| single.get(key))
            .and_then(scalar_to_string)
            .map(|s| vec![s]),
    }
}

/// Build the `aws <service> <verb> [<flag> <captured>]*` command line
/// from the currently-typed tokens. For each flag in `lookup_flags`,
/// scan the tokens for an exact match and grab the next token as its
/// value — mirroring the upstream closure's `tokens.indexOf(flag)`.
/// Flags whose token doesn't appear yet are dropped (the closure
/// would also call aws without them).
fn build_aws_list_command(
    service: &str,
    verb: &str,
    lookup_flags: &[String],
    tokens: &[Annotation],
) -> Vec<String> {
    let mut cmd: Vec<String> = vec!["aws".into(), service.into(), verb.into()];
    for flag in lookup_flags {
        if let Some(i) = tokens.iter().position(|t| t.text == *flag) {
            if let Some(next) = tokens.get(i + 1) {
                cmd.push(flag.clone());
                cmd.push(next.text.clone());
            }
        }
    }
    cmd
}

// ---------------------------------------------------------------------------
// Well-known generator: package.json scripts (npm/yarn/pnpm/bun/rushx/nr)
// ---------------------------------------------------------------------------

type PackageJsonCacheMap =
    HashMap<std::path::PathBuf, (std::time::SystemTime, Vec<(String, String)>)>;

static PACKAGE_JSON_CACHE: std::sync::LazyLock<std::sync::Mutex<PackageJsonCacheMap>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Walk up from CWD until a `package.json` is found, parse it, and
/// return its `scripts` entries as `(name, command)`. Cached per file
/// path keyed on mtime — re-parse only when the file is rewritten.
fn package_json_scripts(cwd: Option<&std::path::Path>) -> Option<Vec<(String, String)>> {
    if let Some(p) = cwd {
        return package_json_scripts_in(p);
    }
    let fallback = std::env::current_dir().ok()?;
    package_json_scripts_in(&fallback)
}

fn package_json_scripts_in(start: &std::path::Path) -> Option<Vec<(String, String)>> {
    let path = find_package_json(start)?;
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    if let Ok(cache) = PACKAGE_JSON_CACHE.lock() {
        if let Some((stamp, scripts)) = cache.get(&path) {
            if *stamp == mtime {
                return Some(scripts.clone());
            }
        }
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let scripts_obj = v.get("scripts")?.as_object()?;
    let mut scripts: Vec<(String, String)> = scripts_obj
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect();
    scripts.sort_by(|a, b| a.0.cmp(&b.0));
    if let Ok(mut cache) = PACKAGE_JSON_CACHE.lock() {
        cache.insert(path, (mtime, scripts.clone()));
    }
    Some(scripts)
}

/// Collect the union of `dependencies` / `devDependencies` /
/// `optionalDependencies` keys from the nearest `package.json` above
/// `cwd`. Returns `(name, kind)` pairs where kind is the description
/// label (`dependency`, `devDependency`, `optionalDependency`).
fn package_json_deps(cwd: Option<&std::path::Path>) -> Option<Vec<(String, &'static str)>> {
    let start = cwd
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())?;
    let path = find_package_json(&start)?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for (key, label) in &[
        ("dependencies", "dependency"),
        ("devDependencies", "devDependency"),
        ("optionalDependencies", "optionalDependency"),
    ] {
        if let Some(obj) = v.get(*key).and_then(|x| x.as_object()) {
            for k in obj.keys() {
                out.push((k.clone(), *label));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    Some(out)
}

/// Run `cargo metadata --format-version 1 --no-deps` in `cwd`, walk
/// `packages[*].targets[*]`, optionally filter by `kind`, and return
/// `(name, kind, src_path_relative_to_cwd)` tuples. Cached via the
/// shared `cached_template_generator` (5s TTL).
///
/// Recovery for the upstream `targetGenerator` closure in
/// vendor/withfig-autocomplete/src/cargo.ts — 78 unresolved customs.
fn cargo_targets(
    cwd: Option<&std::path::Path>,
    kind_filter: Option<&str>,
) -> Option<Vec<(String, String, String)>> {
    let start = cwd
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())?;
    // Run cargo metadata in cwd. Bypass cached_template_generator's
    // JSON-auto-extract path (which would flatten `packages[*]` into
    // candidate strings) and parse the raw blob ourselves — we need
    // the nested `packages[*].targets[*].kind` structure.
    let raw = cached_cargo_metadata(&start)?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let packages = v.get("packages")?.as_array()?;
    let mut out: Vec<(String, String, String)> = Vec::new();
    for pkg in packages {
        let targets = pkg.get("targets").and_then(|t| t.as_array());
        let Some(targets) = targets else { continue };
        for t in targets {
            let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let src = t.get("src_path").and_then(|x| x.as_str()).unwrap_or("");
            let kinds: Vec<String> = t
                .get("kind")
                .and_then(|k| k.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(filter) = kind_filter {
                if !kinds.iter().any(|k| k == filter) {
                    continue;
                }
            }
            if name.is_empty() {
                continue;
            }
            let kind_label = kinds.first().cloned().unwrap_or_else(|| "target".into());
            let rel = src
                .strip_prefix(&format!("{}/", start.display()))
                .unwrap_or(src)
                .to_string();
            out.push((name.to_string(), kind_label, rel));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    Some(out)
}

/// Per-cwd cache for `cargo metadata` raw output. Keyed by cwd
/// (canonicalized), TTL 5s. Lives separately from
/// [`GENERATOR_CACHE`] because that cache splits stdout by lines
/// and routes `{`-prefixed payloads through `extract_json_candidates`
/// — both transforms would destroy the nested
/// `packages[*].targets[*]` structure cargo_targets needs.
///
/// Bounded with the same LRU policy as `GENERATOR_CACHE`
/// ([`GENERATOR_CACHE_MAX`] entries, oldest-evicted on overflow)
/// so a long-running daemon that the user `cd`s through dozens of
/// cargo workspaces doesn't leak.
static CARGO_METADATA_CACHE: std::sync::LazyLock<
    std::sync::Mutex<HashMap<std::path::PathBuf, (std::time::Instant, String)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn cached_cargo_metadata(cwd: &std::path::Path) -> Option<String> {
    use std::process::{Command, Stdio};
    let canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if let Ok(cache) = CARGO_METADATA_CACHE.lock() {
        if let Some((stamp, blob)) = cache.get(&canon) {
            if stamp.elapsed() < GENERATOR_CACHE_TTL {
                return Some(blob.clone());
            }
        }
    }
    let child = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(&canon)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let buf = spawn_with_timeout(child, 65_536)?;
    if buf.is_empty() {
        return None;
    }
    let blob = String::from_utf8_lossy(&buf).into_owned();
    if let Ok(mut cache) = CARGO_METADATA_CACHE.lock() {
        if cache.len() >= GENERATOR_CACHE_MAX {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(canon, (std::time::Instant::now(), blob.clone()));
    }
    Some(blob)
}

/// Extract `{a|b|c}` enum-list suggestions from a Fig description
/// string. Many vendor specs document enum values inline (e.g. `gh
/// pr list --state "Filter by state: {open|closed|merged|all}"`)
/// without populating `arg.suggestions[]`. Recovering them here
/// turns a dead enum into useful completion.
///
/// Conservative rules to avoid false positives:
/// - Braces must contain ≥2 pipe-separated entries
/// - Each entry: 1+ chars from `[A-Za-z0-9_/.-]`
/// - Pipes and entries only — anything else (spaces, equals, etc.)
///   disqualifies the candidate group
///
/// Returns the first matching group only; if a description has
/// multiple enum lists, the first one wins (matches how a human
/// would scan-read the doc string).
fn extract_enum_from_description(desc: &str) -> Option<Vec<String>> {
    let bytes = desc.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            if j < bytes.len() {
                let inner = &desc[i + 1..j];
                if inner.contains('|') {
                    let parts: Vec<&str> = inner.split('|').map(str::trim).collect();
                    let ok = parts.len() >= 2
                        && parts.iter().all(|p| {
                            !p.is_empty()
                                && p.chars().all(|c| {
                                    c.is_ascii_alphanumeric()
                                        || c == '_'
                                        || c == '/'
                                        || c == '.'
                                        || c == '-'
                                })
                        });
                    if ok {
                        return Some(parts.into_iter().map(|s| s.to_string()).collect());
                    }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// Clamp `cursor` down to the nearest valid UTF-8 char boundary at or
/// before the requested byte offset. Prevents the `byte index is not
/// a char boundary` panic when zsh hands us a `$CURSOR` that lands
/// mid-glyph (e.g. Hangul / emoji / CJK input).
fn clamp_cursor_to_char_boundary(line: &str, cursor: usize) -> usize {
    let mut c = cursor.min(line.len());
    while c > 0 && !line.is_char_boundary(c) {
        c -= 1;
    }
    c
}

// ---------------------------------------------------------------------------
// Well-known generator: zoxide directory history (z, zoxide)
// ---------------------------------------------------------------------------

/// Rank zoxide rows against the typed query. `rows` arrive score-desc
/// (frecency). Groups, in order of intent: folder-name prefix hits, then
/// folder-name substring hits, then path-only hits (the name doesn't
/// match but a parent directory does). Frecency order is preserved
/// within each group, and non-matching rows drop out. This is what makes
/// `z enc` surface `encl` (a name-prefix hit) above `app`/`apps`, whose
/// only match is the shared `.../encl/...` parent path.
fn rank_zoxide_matches(
    rows: Vec<(String, String, f64)>,
    query: &str,
) -> Vec<(String, String, f64)> {
    let needle = query.to_lowercase();
    let (mut name_prefix, mut name_substr, mut path_only) =
        (Vec::new(), Vec::new(), Vec::new());
    for row in rows {
        let name_lc = row.0.to_lowercase();
        if needle.is_empty() || name_lc.starts_with(&needle) {
            name_prefix.push(row);
        } else if name_lc.contains(&needle) {
            name_substr.push(row);
        } else if row.1.to_lowercase().contains(&needle) {
            path_only.push(row);
        }
    }
    name_prefix
        .into_iter()
        .chain(name_substr)
        .chain(path_only)
        .collect()
}

/// Resolve the zoxide / zsh-z directory history. Tries `zoxide query
/// --list --score` first (200ms cap, cached); on failure falls back
/// to the zsh-z `~/.z` flat file (or `$_Z_DATA` / `$ZSHZ_DATA` env
/// override). Returns `(folder_name, full_path, score)` tuples
/// sorted by descending score. The folder_name is the last path
/// segment — what the user usually wants to insert.
fn zoxide_query() -> Option<Vec<(String, String, f64)>> {
    if let Some(rows) = zoxide_via_command() {
        if !rows.is_empty() {
            return Some(sorted_by_score(rows));
        }
    }
    zoxide_via_z_file().map(sorted_by_score)
}

fn zoxide_via_command() -> Option<Vec<(String, String, f64)>> {
    let key = vec![
        "zoxide".to_string(),
        "query".to_string(),
        "--list".to_string(),
        "--score".to_string(),
    ];
    // zoxide keeps a single global db — cwd-independent.
    let lines = cached_template_generator(&key, None)?;
    let mut rows: Vec<(String, String, f64)> = Vec::new();
    for line in lines {
        // Each line: "<spaces><score> <path>".
        let trimmed = line.trim_start();
        let mut split = trimmed.splitn(2, char::is_whitespace);
        let score_str = split.next()?;
        let path = split.next()?.trim().to_string();
        let score: f64 = score_str.parse().ok()?;
        rows.push((folder_name(&path), path, score));
    }
    Some(rows)
}

fn zoxide_via_z_file() -> Option<Vec<(String, String, f64)>> {
    let path = z_history_file()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut rows: Vec<(String, String, f64)> = Vec::new();
    for line in raw.lines() {
        // zsh-z / z.sh format: "<path>|<score>|<unixtime>".
        let mut parts = line.splitn(3, '|');
        let p = parts.next()?.trim().to_string();
        let s = parts.next()?.trim();
        let score: f64 = s.parse().ok()?;
        rows.push((folder_name(&p), p, score));
    }
    Some(rows)
}

fn z_history_file() -> Option<std::path::PathBuf> {
    if let Ok(v) = std::env::var("ZSHZ_DATA") {
        if !v.is_empty() {
            return Some(std::path::PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("_Z_DATA") {
        if !v.is_empty() {
            return Some(std::path::PathBuf::from(v));
        }
    }
    let home = std::env::var("HOME").ok()?;
    let candidate = std::path::PathBuf::from(home).join(".z");
    candidate.exists().then_some(candidate)
}

fn folder_name(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn sorted_by_score(mut rows: Vec<(String, String, f64)>) -> Vec<(String, String, f64)> {
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    rows
}

// ---------------------------------------------------------------------------
// `template: history` — last N unique words from $HISTFILE.
// ---------------------------------------------------------------------------

const HISTORY_MAX_RETURN: usize = 200;
const HISTORY_TAIL_LINES: usize = 2000;

/// Pull the last ~2000 lines from the user's shell history file and
/// return the unique whitespace tokens contained, last-seen first.
/// Looks at `$HISTFILE`, then `~/.zsh_history`, then `~/.bash_history`.
fn shell_history_entries() -> Option<Vec<String>> {
    let path = history_file()?;
    // zsh stores history with `\xNN`-escaped bytes that aren't valid
    // UTF-8 byte sequences (e.g. Korean chars under `setopt
    // EXTENDED_HISTORY`). Read raw bytes and lossy-decode — losing
    // bad sequences as `U+FFFD` is fine; we only emit ASCII tokens.
    let bytes = std::fs::read(&path).ok()?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    // Iterate newest-first.
    let lines: Vec<&str> = raw.lines().rev().take(HISTORY_TAIL_LINES).collect();
    for line in lines {
        // zsh extended-history lines look like `: 1700000000:0;cmd args`.
        // Trim the metadata prefix when present.
        let payload = match line.find(';') {
            Some(i) if line.starts_with(": ") => &line[i + 1..],
            _ => line,
        };
        for token in payload.split_whitespace() {
            if seen.insert(token.to_string()) {
                out.push(token.to_string());
                if out.len() >= HISTORY_MAX_RETURN {
                    return Some(out);
                }
            }
        }
    }
    Some(out)
}

fn history_file() -> Option<std::path::PathBuf> {
    if let Ok(v) = std::env::var("HISTFILE") {
        if !v.is_empty() {
            let p = std::path::PathBuf::from(v);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let home = std::env::var_os("HOME")?;
    let home = std::path::PathBuf::from(home);
    for name in &[".zsh_history", ".bash_history"] {
        let p = home.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Well-known generator: SSH host enumeration (ssh, scp, sftp, mosh, rsync)
// ---------------------------------------------------------------------------

/// Collect SSH hosts from `~/.ssh/known_hosts` + `~/.ssh/config` (with
/// `Include` directive support, max one level deep). Dedup, sort, no
/// score. Returns `None` only when `HOME` is unset; an empty Vec
/// otherwise (caller surfaces that as "no completions").
fn ssh_hosts() -> Option<Vec<String>> {
    let home = std::env::var_os("HOME")?;
    let ssh_dir = std::path::PathBuf::from(home).join(".ssh");
    let mut hosts: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    ssh_hosts_from_known(&ssh_dir.join("known_hosts"), &mut hosts);
    ssh_hosts_from_config(&ssh_dir.join("config"), &ssh_dir, &mut hosts, 0);
    Some(hosts.into_iter().collect())
}

fn ssh_hosts_from_known(path: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        // `known_hosts` lines: `<hosts> <key-type> <key>` where `<hosts>`
        // is a comma-list of `host` / `host,host:port` / `[host]:port`
        // / `|1|salt|hash` (hashed — skip). Skip comments + empty.
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("|1|") {
            continue;
        }
        let Some(host_field) = trimmed.split_whitespace().next() else {
            continue;
        };
        for entry in host_field.split(',') {
            let cleaned = entry
                .trim_start_matches('[')
                .split([']', ':'])
                .next()
                .unwrap_or(entry)
                .trim();
            if !cleaned.is_empty() && !cleaned.contains('*') {
                out.insert(cleaned.to_string());
            }
        }
    }
}

fn ssh_hosts_from_config(
    path: &std::path::Path,
    ssh_dir: &std::path::Path,
    out: &mut std::collections::BTreeSet<String>,
    depth: usize,
) {
    if depth > 1 {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let rest = parts.next().unwrap_or("").trim();
        if key == "host" {
            for entry in rest.split_whitespace() {
                if !entry.contains('*') && !entry.contains('?') {
                    out.insert(entry.to_string());
                }
            }
        } else if key == "include" {
            for inc in rest.split_whitespace() {
                let p = if inc.starts_with('/') {
                    std::path::PathBuf::from(inc)
                } else {
                    ssh_dir.join(inc)
                };
                ssh_hosts_from_config(&p, ssh_dir, out, depth + 1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Well-known generator: Makefile target enumeration (`make`)
// ---------------------------------------------------------------------------

/// Parse `Makefile` / `makefile` / `GNUmakefile` in the current
/// working directory and return target names. Recognises lines of
/// the form `<name>:` where `<name>` is composed of identifier-safe
/// characters — same surface area as Fig's `listTargets` closure but
/// without booting Node.
fn makefile_targets(cwd: Option<&std::path::Path>) -> Option<Vec<String>> {
    let dir = cwd
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())?;
    let path = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut targets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in raw.lines() {
        // Skip recipe lines (start with a tab) and continuation chunks.
        if line.starts_with('\t') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let head = &line[..colon];
        // Skip `target = value` style variables — `:=` rules out
        // direct-set, and a leading `#` is a comment.
        if head.contains('=') || head.trim_start().starts_with('#') {
            continue;
        }
        for raw_target in head.split_whitespace() {
            if is_makefile_target_name(raw_target) && !raw_target.starts_with('.') {
                targets.insert(raw_target.to_string());
            }
        }
    }
    Some(targets.into_iter().collect())
}

fn is_makefile_target_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
}

// ---------------------------------------------------------------------------
// Well-known generator: man page enumeration (`man`)
// ---------------------------------------------------------------------------

/// Walk the user's `MANPATH` (or the common defaults) for `manN/*`
/// entries and return the bare page name (no section suffix, no
/// `.gz`). Matches the surface area of Fig's `generateManualPages`
/// closure without spawning `man -k`.
fn man_pages() -> Option<Vec<String>> {
    let roots = man_path_roots();
    if roots.is_empty() {
        return None;
    }
    let mut pages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for root in roots {
        let Ok(sections) = std::fs::read_dir(&root) else {
            continue;
        };
        for sec in sections.flatten() {
            let name = sec.file_name();
            let Some(s) = name.to_str() else {
                continue;
            };
            if !s.starts_with("man") || s.len() < 4 {
                continue;
            }
            let Ok(files) = std::fs::read_dir(sec.path()) else {
                continue;
            };
            for f in files.flatten() {
                let fname = f.file_name();
                let Some(fs) = fname.to_str() else { continue };
                if let Some(page) = man_page_stem(fs) {
                    pages.insert(page);
                }
            }
        }
    }
    Some(pages.into_iter().collect())
}

fn man_page_stem(file_name: &str) -> Option<String> {
    let trimmed = file_name.strip_suffix(".gz").unwrap_or(file_name);
    // `git.1` / `printf.3` / `man.1posix` — strip the last dot and
    // anything after.
    let dot = trimmed.rfind('.')?;
    let stem = &trimmed[..dot];
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn man_path_roots() -> Vec<std::path::PathBuf> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("MANPATH") {
        for p in v.split(':') {
            if !p.is_empty() {
                roots.push(std::path::PathBuf::from(p));
            }
        }
    }
    for fallback in [
        "/usr/share/man",
        "/usr/local/share/man",
        "/opt/homebrew/share/man",
        "/Library/Developer/CommandLineTools/usr/share/man",
    ] {
        let p = std::path::PathBuf::from(fallback);
        if p.exists() && !roots.contains(&p) {
            roots.push(p);
        }
    }
    roots
}

// ---------------------------------------------------------------------------
// Well-known generator: filepaths / folders (cd, cat, ls, …)
// ---------------------------------------------------------------------------

/// List directory entries matching the trailing-basename portion of
/// `prefix`. Honours `folders_only` (e.g. `cd` uses showFolders=only).
/// `(insertion, display, description, icon)` row tuple emitted by
/// the filesystem walker. Insertion preserves the user's typed
/// directory prefix so `cd ./fo<Tab>` → `cd ./encl/`, not `encl/`.
type FilepathRow = (String, String, Option<String>, Option<String>);

fn filepaths_at(
    cwd: Option<&std::path::Path>,
    prefix: &str,
    folders_only: bool,
) -> Option<Vec<FilepathRow>> {
    // Split prefix into (dir_part_preserve_trailing_slash, basename_filter).
    let (dir_part, filter) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let resolved = resolve_filepaths_root(cwd, dir_part)?;
    let entries = std::fs::read_dir(&resolved).ok()?;
    let mut out: Vec<FilepathRow> = Vec::new();
    for e in entries.flatten() {
        let name_os = e.file_name();
        let Some(name) = name_os.to_str() else {
            continue;
        };
        if !name.starts_with(filter) {
            continue;
        }
        // Skip dotfiles unless user explicitly typed a leading dot.
        if name.starts_with('.') && !filter.starts_with('.') {
            continue;
        }
        let ft = e.file_type().ok();
        let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        if folders_only && !is_dir {
            continue;
        }
        let trailing = if is_dir { "/" } else { "" };
        let insertion = format!("{dir_part}{name}{trailing}");
        let display = format!("{name}{trailing}");
        let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
        let desc = filepaths_desc(&e, is_dir, is_symlink);
        // Per-row icon: 🔗 for symlinks (checked first; a symlink
        // pointing at a dir still gets the link glyph), 📁 for
        // dirs, 📄 for regular files. All 4-byte UTF-8, pass
        // sanitize_icon's ≤4 byte gate.
        let icon = if is_symlink {
            Some("🔗".to_string())
        } else if is_dir {
            Some("📁".to_string())
        } else {
            Some("📄".to_string())
        };
        out.push((insertion, display, desc, icon));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Some(out)
}

fn filepaths_desc(entry: &std::fs::DirEntry, is_dir: bool, is_symlink: bool) -> Option<String> {
    if is_symlink {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            return Some(format!("→ {}", target.display()));
        }
        return Some("symlink".into());
    }
    if is_dir {
        // Smart fallback for cd / z: every row would otherwise just
        // say "dir" — uninformative. Show item count when cheap
        // (~50µs per read_dir on typical sizes). Skip on read error
        // (perm denied / unreadable) and fall back to "dir".
        return Some(dir_summary(&entry.path()));
    }
    let meta = entry.metadata().ok()?;
    Some(human_size(meta.len()))
}

/// One-line summary for a directory entry. Reads the dir to count
/// visible children (excluding dotfiles). Cap the iteration at 200
/// entries to keep latency bounded for huge dirs (node_modules).
fn dir_summary(path: &std::path::Path) -> String {
    let Ok(rd) = std::fs::read_dir(path) else {
        return "dir".into();
    };
    let mut count: u32 = 0;
    let mut truncated = false;
    for (i, e) in rd.flatten().enumerate() {
        if i >= 200 {
            truncated = true;
            break;
        }
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with('.') {
            continue;
        }
        count += 1;
    }
    if count == 0 {
        "empty".into()
    } else if truncated {
        format!("{count}+ items")
    } else if count == 1 {
        "1 item".into()
    } else {
        format!("{count} items")
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}{}", UNITS[0])
    } else if v >= 100.0 {
        format!("{v:.0}{}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.2}{}", UNITS[u])
    }
}

/// Resolve a (possibly relative, possibly tilde-prefixed) directory
/// string against the client's cwd. Returns the canonical filesystem
/// path to read, or `None` if neither cwd nor `HOME` is known.
fn resolve_filepaths_root(
    cwd: Option<&std::path::Path>,
    dir_part: &str,
) -> Option<std::path::PathBuf> {
    if dir_part.is_empty() {
        return cwd
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok());
    }
    if dir_part.starts_with('/') {
        return Some(std::path::PathBuf::from(dir_part));
    }
    if let Some(rest) = dir_part.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(std::path::PathBuf::from(home).join(rest));
    }
    if dir_part == "~" {
        let home = std::env::var("HOME").ok()?;
        return Some(std::path::PathBuf::from(home));
    }
    let base = cwd
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())?;
    Some(base.join(dir_part))
}

fn find_package_json(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut cur = start;
    loop {
        let candidate = cur.join("package.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = cur.parent()?;
    }
}

/// Trim shell-list cosmetics from a generator output line.
/// Covers the cases that cost the most to leave raw:
/// - leading ANSI color escapes (e.g. `\x1b[32malice\x1b[0m`)
/// - leading whitespace
/// - leading `* ` (git branch's current-branch marker)
/// - leading `+ ` (git worktree's locked-worktree marker)
/// - `remotes/` prefix on `git branch -a` remote-tracking refs, so the
///   usable ref (`origin/main`) is what gets inserted, not the raw
///   `remotes/origin/main` (Fig #2572 / #2501)
/// - the symbolic HEAD pointer line (`remotes/origin/HEAD -> origin/main`),
///   which is not a checkout target — dropped to an empty string so the
///   caller filters it out
/// - detached-HEAD / mid-rebase pseudo-entries (`(no branch, rebasing
///   feature-x)`, `(HEAD detached at abc1234)`), which are not checkout
///   targets either — also dropped (Fig #2463)
///
/// Keeps the rest of the line untouched — anything more aggressive
/// belongs in a JS post-process hook (Tier C, deferred).
fn sanitize_generator_line(raw: &str) -> String {
    let mut s = strip_ansi(raw);
    s = s.trim().to_string();
    if let Some(rest) = s.strip_prefix("* ") {
        s = rest.trim_start().to_string();
    } else if let Some(rest) = s.strip_prefix("+ ") {
        s = rest.trim_start().to_string();
    }
    // git refnames never start with `(`, so a `(`-leading line is one of
    // git's parenthesised pseudo-branches (detached HEAD, rebase state).
    if s.starts_with('(') || s.contains(" -> ") {
        return String::new();
    }
    if let Some(rest) = s.strip_prefix("remotes/") {
        s = rest.to_string();
    }
    s
}

/// Remove ANSI CSI escape sequences (`\x1b[...m` etc.) without
/// pulling in a regex dep. Iterates bytes; preserves UTF-8 by
/// only skipping ESC + bracket-form sequences.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            // Drain until letter (CSI final byte: 0x40..0x7e).
            for nc in chars.by_ref() {
                if ('@'..='~').contains(&nc) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn name_or_aliases_match(name: &str, aliases: &[String], prefix: &str, mode: MatchMode) -> bool {
    if matches_name(name, prefix, mode) {
        return true;
    }
    aliases.iter().any(|a| matches_name(a, prefix, mode))
}

/// Build the workspace fixture path — used by `nerv-daemon` e2e tests
/// to point at hand-authored JSON specs without touching real
/// `~/Library/Caches/nerv/specs/`.
pub fn workspace_fixture_specs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("specs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_parser::{Arg, Opt, Subcommand};

    fn git_min() -> Spec {
        Subcommand {
            name: "git".into(),
            description: Some("VCS".into()),
            subcommands: vec![
                Subcommand {
                    name: "status".into(),
                    description: Some("status".into()),
                    ..Default::default()
                },
                Subcommand {
                    name: "checkout".into(),
                    aliases: vec!["co".into()],
                    args: vec![Arg {
                        name: Some("branch".into()),
                        suggestions: vec![
                            crate::spec_parser::RawSuggestion {
                                name: "main".into(),
                                ..Default::default()
                            },
                            crate::spec_parser::RawSuggestion {
                                name: "dev".into(),
                                ..Default::default()
                            },
                            crate::spec_parser::RawSuggestion {
                                name: "feature/x".into(),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                Subcommand {
                    name: "commit".into(),
                    options: vec![Opt {
                        names: vec!["-m".into(), "--message".into()],
                        args: vec![Arg::default()],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn registry_with(spec: Spec) -> SpecRegistry {
        let r = SpecRegistry::empty();
        r.insert(spec);
        r
    }

    #[test]
    fn empty_input_returns_empty() {
        let r = complete("", 0, &registry_with(git_min()));
        assert!(r.items.is_empty());
    }

    #[test]
    fn unknown_binary_returns_empty_with_reason() {
        let r = complete("unknownbin foo", 14, &registry_with(git_min()));
        assert!(r.items.is_empty());
        assert!(r.reason.is_some());
    }

    #[test]
    fn git_space_emits_all_subcommands() {
        let r = complete("git ", 4, &registry_with(git_min()));
        // Assert on `insertion` (the bare name), not `display`: display
        // may carry a Fig-style arg hint ("checkout <branch>").
        let names: Vec<_> = r.items.iter().map(|s| s.insertion.as_str()).collect();
        assert_eq!(names, ["checkout", "commit", "status"]);
    }

    #[test]
    fn git_co_filters_by_prefix_in_subcommand_names() {
        let r = complete("git co", 6, &registry_with(git_min()));
        // `co` matches checkout (via alias starts_with) and commit (primary).
        let names: Vec<_> = r.items.iter().map(|s| s.insertion.as_str()).collect();
        assert_eq!(names, ["checkout", "commit"]);
    }

    #[test]
    fn zoxide_ranks_name_prefix_then_substring_then_path() {
        // Rows come score-desc (frecency). The `enc` query:
        //   app       — name has no `enc`, path does (parent `encl`)
        //   evidence  — name substring `evidENCe`
        //   encl      — name prefix, lower raw score than the others
        //   ios       — path-only
        let rows = vec![
            ("app".to_string(), "/w/encl/apps/app".to_string(), 100.0),
            ("evidence".to_string(), "/w/encl/evidence".to_string(), 90.0),
            ("encl".to_string(), "/w/encl".to_string(), 80.0),
            ("ios".to_string(), "/w/encl/ios".to_string(), 70.0),
        ];
        let names: Vec<_> = rank_zoxide_matches(rows, "enc")
            .into_iter()
            .map(|r| r.0)
            .collect();
        // Name prefix (encl) beats name substring (evidence) beats
        // path-only (app before ios by score), regardless of raw score.
        assert_eq!(names, ["encl", "evidence", "app", "ios"]);
    }

    #[test]
    fn zoxide_empty_query_preserves_frecency_order() {
        let rows = vec![
            ("b".to_string(), "/b".to_string(), 30.0),
            ("a".to_string(), "/a".to_string(), 20.0),
        ];
        let names: Vec<_> = rank_zoxide_matches(rows, "")
            .into_iter()
            .map(|r| r.0)
            .collect();
        // No re-alphabetising — score order (b before a) is kept.
        assert_eq!(names, ["b", "a"]);
    }

    #[test]
    fn arg_hint_formats_optional_required_variadic() {
        use crate::spec_parser::Arg;
        let mk = |name: &str, opt: bool, var: bool| Arg {
            name: Some(name.into()),
            is_optional: opt,
            is_variadic: var,
            ..Default::default()
        };
        // git push: two optional args → "[remote] [branch]".
        assert_eq!(
            arg_hint(&[mk("remote", true, false), mk("branch", true, false)]),
            "[remote] [branch]"
        );
        // required → angle brackets; variadic → trailing "...".
        assert_eq!(arg_hint(&[mk("file", false, false)]), "<file>");
        assert_eq!(arg_hint(&[mk("path", false, true)]), "<path...>");
        assert_eq!(arg_hint(&[mk("arg", true, true)]), "[arg...]");
        // no named args → empty; unnamed args skipped.
        assert_eq!(arg_hint(&[]), "");
        assert_eq!(arg_hint(&[Arg { name: None, ..Default::default() }]), "");
    }

    #[test]
    fn subcommand_display_carries_arg_hint_insertion_stays_bare() {
        // git_min's `checkout` has a (required, in-fixture) `branch` arg.
        let r = complete("git ", 4, &registry_with(git_min()));
        let checkout = r
            .items
            .iter()
            .find(|s| s.insertion == "checkout")
            .expect("checkout emitted");
        assert_eq!(checkout.insertion, "checkout");
        assert_eq!(checkout.display, "checkout <branch>");
        // A no-arg subcommand keeps a bare display.
        let status = r.items.iter().find(|s| s.insertion == "status").unwrap();
        assert_eq!(status.display, "status");
    }

    #[test]
    fn exact_token_match_is_dropped_as_noop() {
        // `git status` fully typed: the `status` subcommand is a no-op
        // completion (insertion == typed token) and must not appear.
        let r = complete("git status", 10, &registry_with(git_min()));
        assert!(
            !r.items.iter().any(|s| s.insertion == "status"),
            "exact match should be dropped, got: {:?}",
            r.items.iter().map(|s| &s.insertion).collect::<Vec<_>>()
        );
        // A partial token still completes: `git stat` keeps `status`.
        let r2 = complete("git stat", 8, &registry_with(git_min()));
        assert!(r2.items.iter().any(|s| s.insertion == "status"));
    }

    #[test]
    fn git_commit_emits_options() {
        let r = complete("git commit ", 11, &registry_with(git_min()));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["--message", "-m"]);
    }

    #[test]
    fn git_commit_dash_dash_filters_long_options() {
        let r = complete("git commit --", 13, &registry_with(git_min()));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["--message"]);
    }

    #[test]
    fn git_checkout_arg_emits_static_suggestions() {
        let r = complete("git checkout ", 13, &registry_with(git_min()));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["dev", "feature/x", "main"]);
    }

    #[test]
    fn registry_load_dir_handles_missing_dir() {
        let (r, errs) = SpecRegistry::load_dir(Path::new("/tmp/nerv-nonexistent-xyz"));
        assert!(r.is_empty());
        assert!(errs.is_empty());
    }

    #[test]
    fn registry_load_dir_picks_up_workspace_fixtures() {
        let dir = workspace_fixture_specs_dir();
        let (r, errs) = SpecRegistry::load_dir(&dir);
        assert!(errs.is_empty(), "fixture load errors: {errs:?}");
        for name in [
            "git", "echo", "docker", "kubectl", "npm", "cargo", "gh", "brew", "make",
        ] {
            assert!(r.lookup(name).is_some(), "fixture missing: {name}");
        }
    }

    #[test]
    fn template_generator_emits_stdout_lines() {
        use crate::spec_parser::{Arg, Generator, Subcommand};
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("opt".into()),
                generators: vec![Generator::Template {
                    script: vec!["/usr/bin/printf".into(), "alpha\nbeta\ngamma\n".into()],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = complete("x ", 2, &registry_with(spec));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }

    /// `cargo run -p <Tab>` regression: a `ScriptWithJsonPath` generator
    /// whose script emits a nested JSON object (`{packages:[…], …}`) must
    /// navigate `parent_key`/`id_field` over the **raw** stdout. The arm
    /// used to route through `cached_template_generator`, which intercepts
    /// `{`-leading output with `extract_json_candidates` and guesses the
    /// wrong array — leaving the explicit json-path with nothing and the
    /// completion empty.
    #[test]
    fn script_with_json_path_navigates_nested_object() {
        use crate::spec_parser::{Arg, Generator, Subcommand};
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("pkg".into()),
                generators: vec![Generator::ScriptWithJsonPath {
                    script: vec![
                        "/usr/bin/printf".into(),
                        // Mirrors `cargo metadata`: the target array is
                        // nested under `packages`, not at top level.
                        r#"{"packages":[{"name":"alpha"},{"name":"beta"}],"workspace_members":["x"]}"#
                            .into(),
                    ],
                    parent_key: "packages".into(),
                    id_field: Some("name".into()),
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = complete("x ", 2, &registry_with(spec));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    /// `kubectl get pods -n <Tab>` regression: a persistent ROOT
    /// option bound mid-chain must dispatch to ITS arg's generator,
    /// not fall through to the subcommand's positional slot. The
    /// dispatch in `complete_in` used to look at `current.options`
    /// only, so root-declared persistent options silently lost their
    /// generators after any subcommand.
    #[test]
    fn persistent_root_option_arg_generator_runs_mid_chain() {
        use crate::spec_parser::{Arg, Generator, Opt, Subcommand};
        let spec = Subcommand {
            name: "k".into(),
            options: vec![Opt {
                names: vec!["-n".into(), "--namespace".into()],
                is_persistent: true,
                args: vec![Arg {
                    name: Some("namespace".into()),
                    generators: vec![Generator::Template {
                        script: vec!["/usr/bin/printf".into(), "ns-one\nns-two\n".into()],
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            subcommands: vec![Subcommand {
                name: "get".into(),
                args: vec![Arg {
                    name: Some("type".into()),
                    generators: vec![Generator::Template {
                        script: vec!["/usr/bin/printf".into(), "pos-only\n".into()],
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let line = "k get -n ";
        let r = complete(line, line.len(), &registry_with(spec));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(
            names,
            ["ns-one", "ns-two"],
            "expected the -n option's generator, not the positional slot"
        );
    }

    // NOTE: NERV_NO_GENERATORS env-var kill switch is documented but
    // not unit-tested — setting/clearing env vars races with parallel
    // tests (cargo's default runner). Manually verified in the daemon
    // smoke-test path. See PRD v0.6 §0.2 for the contract.

    #[test]
    fn sanitize_strips_git_current_branch_marker() {
        assert_eq!(sanitize_generator_line("* main"), "main");
        assert_eq!(sanitize_generator_line("  feature-x"), "feature-x");
        assert_eq!(sanitize_generator_line("+ wt-locked"), "wt-locked");
        assert_eq!(sanitize_generator_line("regular"), "regular");
        assert_eq!(sanitize_generator_line(""), "");
    }

    #[test]
    fn sanitize_strips_remotes_prefix_and_head_pointer() {
        // `git branch -a` lists remote-tracking refs with a `remotes/`
        // prefix; the usable ref drops it (Fig #2572 / #2501).
        assert_eq!(
            sanitize_generator_line("  remotes/origin/main"),
            "origin/main"
        );
        assert_eq!(
            sanitize_generator_line("remotes/upstream/feature-x"),
            "upstream/feature-x"
        );
        // The symbolic HEAD pointer is not a checkout target — dropped.
        assert_eq!(
            sanitize_generator_line("  remotes/origin/HEAD -> origin/main"),
            ""
        );
        // Detached-HEAD / mid-rebase pseudo-branches dropped (Fig #2463).
        assert_eq!(
            sanitize_generator_line("* (no branch, rebasing feature-x)"),
            ""
        );
        assert_eq!(sanitize_generator_line("(HEAD detached at abc1234)"), "");
        // Local branches are untouched.
        assert_eq!(sanitize_generator_line("* main"), "main");
        assert_eq!(sanitize_generator_line("feature/foo"), "feature/foo");
    }

    #[test]
    fn cap_git_history_bounds_unbounded_walks() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // `rev-list --all` / bare `log` get a max-count appended (#2607).
        assert_eq!(
            cap_git_history(&s(&["git", "rev-list", "--all", "--oneline"])),
            s(&["git", "rev-list", "--all", "--oneline", "--max-count=1000"])
        );
        assert_eq!(
            cap_git_history(&s(&["git", "--no-optional-locks", "log", "--oneline"])).last(),
            Some(&"--max-count=1000".to_string())
        );
        // Already-bounded walks are left alone (-n, --max-count=, -5).
        assert_eq!(
            cap_git_history(&s(&["git", "log", "-n", "5", "--oneline"])),
            s(&["git", "log", "-n", "5", "--oneline"])
        );
        assert_eq!(
            cap_git_history(&s(&["git", "log", "--max-count=3"])),
            s(&["git", "log", "--max-count=3"])
        );
        assert_eq!(
            cap_git_history(&s(&["git", "log", "-5"])),
            s(&["git", "log", "-5"])
        );
        // A `--` pathspec separator means appending would be read as a
        // path — leave untouched.
        assert_eq!(
            cap_git_history(&s(&["git", "log", "--oneline", "--", "src/"])),
            s(&["git", "log", "--oneline", "--", "src/"])
        );
        // Non-history git commands and non-git commands untouched.
        assert_eq!(
            cap_git_history(&s(&["git", "branch", "-a"])),
            s(&["git", "branch", "-a"])
        );
        assert_eq!(cap_git_history(&s(&["docker", "ps"])), s(&["docker", "ps"]));
    }

    #[test]
    fn split_id_label_extracts_commit_hash() {
        // `git log --oneline` → bare hash inserted, full line displayed
        // (Fig #2606).
        let (ins, disp) = split_id_label("abc1234 fix(cli): handle remotes");
        assert_eq!(ins, "abc1234");
        assert_eq!(disp, "abc1234 fix(cli): handle remotes");
        // Numeric pid id behaviour is preserved.
        assert_eq!(split_id_label("1234 /bin/zsh").0, "1234");
        // Single-column candidates (branches, refs) stay whole — no
        // false-positive splitting on a hash-less first token.
        assert_eq!(split_id_label("main"), ("main".into(), "main".into()));
        assert_eq!(
            split_id_label("origin/feature remote-branch"),
            (
                "origin/feature remote-branch".into(),
                "origin/feature remote-branch".into()
            )
        );
        // A first token with a non-hex letter is not a hash.
        assert_eq!(
            split_id_label("zzghijk subject"),
            ("zzghijk subject".into(), "zzghijk subject".into())
        );
    }

    #[test]
    fn is_commit_hash_bounds() {
        assert!(is_commit_hash("abc1234")); // 7 chars, min
        assert!(is_commit_hash("0123456789abcdef0123456789abcdef01234567")); // 40, max
        assert!(!is_commit_hash("abc123")); // 6 chars, too short
        assert!(!is_commit_hash("ABC1234")); // uppercase — avoid ALL-CAPS words
        assert!(!is_commit_hash("ghi1234")); // g/h/i not hex
    }

    #[test]
    fn sanitize_strips_ansi_color_codes() {
        // git -c color.branch=always branch outputs something like:
        let raw = "\x1b[32m* main\x1b[0m";
        assert_eq!(sanitize_generator_line(raw), "main");
        let raw2 = "\x1b[31malice\x1b[0m";
        assert_eq!(sanitize_generator_line(raw2), "alice");
        let raw3 = "\x1b[1;33;40mfoo\x1b[m";
        assert_eq!(sanitize_generator_line(raw3), "foo");
    }

    #[test]
    fn template_generator_cached_on_repeat() {
        use crate::spec_parser::{Arg, Generator, Subcommand};
        use std::time::Instant;
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("opt".into()),
                generators: vec![Generator::Template {
                    // Unique payload so we don't share a key with other
                    // tests that use the same static cache.
                    script: vec!["/usr/bin/printf".into(), "cache-test-uniq-abc\n".into()],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let reg = registry_with(spec);

        // Cold call — populates cache.
        let _ = complete("x ", 2, &reg);
        // Warm call — should be faster (no spawn).
        let t0 = Instant::now();
        let r = complete("x ", 2, &reg);
        let elapsed = t0.elapsed();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].display, "cache-test-uniq-abc");
        // Spawn would take >1ms; cached path is microseconds. Use a
        // generous 5ms ceiling so this isn't flaky on slow CI.
        assert!(
            elapsed.as_millis() < 5,
            "expected cache hit <5ms, got {elapsed:?}"
        );
    }

    #[test]
    fn template_generator_is_cwd_keyed_and_runs_in_cwd() {
        // Two distinct real dirs; `pwd -P` reads getcwd() (not the stale
        // inherited $PWD), so its output reflects the cwd we spawn in. If
        // the generator ran in nervd's cwd, or the cache keyed on argv
        // alone (Fig #2101 / #2026 / #2268), both calls would match.
        let base = std::env::temp_dir();
        let dir_a = base.join("nerv_cwd_key_a");
        let dir_b = base.join("nerv_cwd_key_b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let script = vec!["/bin/pwd".to_string(), "-P".to_string()];
        let a = cached_template_generator(&script, Some(&dir_a));
        let b = cached_template_generator(&script, Some(&dir_b));
        assert!(a.is_some() && b.is_some(), "pwd should produce output");
        assert_ne!(
            a, b,
            "same argv in different cwd must not share a cache entry"
        );
    }

    #[test]
    fn generator_times_out_instead_of_hanging() {
        // A generator that would only emit after 2s must be killed at the
        // GENERATOR_TIMEOUT_MS bound and return None — never freeze the
        // prompt (the failure mode behind Fig #2102 / #1838). Also assert
        // we return well before the child's 2s, proving the kill fired.
        use std::time::Instant;
        let script = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 2; echo late".to_string(),
        ];
        let t0 = Instant::now();
        let out = execute_template_generator(&script, None);
        let elapsed = t0.elapsed();
        assert!(
            out.is_none(),
            "slow generator must time out to None, got {out:?}"
        );
        assert!(
            elapsed.as_millis() < 1500,
            "must return near the {GENERATOR_TIMEOUT_MS}ms timeout, not wait \
             for the child; got {elapsed:?}"
        );
    }

    #[test]
    fn template_generator_filters_by_prefix() {
        use crate::spec_parser::{Arg, Generator, Subcommand};
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("opt".into()),
                generators: vec![Generator::Template {
                    script: vec![
                        "/usr/bin/printf".into(),
                        "main\ndev\nfeature-a\nfeature-b\n".into(),
                    ],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = complete("x feat", 6, &registry_with(spec));
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["feature-a", "feature-b"]);
    }

    #[test]
    fn package_json_scripts_walks_up_and_parses() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("nerv-pkg-{}", std::process::id()));
        let nested = tmp.join("a/b/c");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            tmp.join("package.json"),
            r#"{"name":"x","scripts":{"build":"tsc","test":"vitest","dev":"vite"}}"#,
        )
        .unwrap();

        let scripts = package_json_scripts_in(&nested).expect("scripts");
        let names: Vec<&str> = scripts.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["build", "dev", "test"]);
        let test_cmd = scripts.iter().find(|(n, _)| n == "test").unwrap();
        assert_eq!(test_cmd.1, "vitest");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn extract_json_top_level_array_of_strings() {
        let raw = r#"["alpha","beta","gamma"]"#;
        let out = extract_json_candidates(raw).expect("parsed");
        assert_eq!(out, ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn extract_json_array_of_objects_picks_name_field() {
        let raw = r#"[{"name":"main","sha":"abc"},{"name":"dev","sha":"def"}]"#;
        let out = extract_json_candidates(raw).expect("parsed");
        assert_eq!(out, ["main", "dev"]);
    }

    #[test]
    fn extract_json_gh_pr_list_picks_number() {
        let raw = r#"[{"number":42,"title":"fix x","state":"OPEN"},{"number":17,"title":"add y"}]"#;
        let out = extract_json_candidates(raw).expect("parsed");
        assert_eq!(out, ["42", "17"]);
    }

    #[test]
    fn extract_json_kubectl_items_wrapper() {
        let raw = r#"{"items":[{"metadata":{"name":"pod-1"}},{"metadata":{"name":"pod-2"}}]}"#;
        let out = extract_json_candidates(raw).expect("parsed");
        assert_eq!(out, ["pod-1", "pod-2"]);
    }

    #[test]
    fn extract_json_malformed_returns_none() {
        assert!(extract_json_candidates("not json at all").is_none());
        // valid JSON, but no array we can find
        assert!(extract_json_candidates(r#"{"foo":"bar"}"#).is_none());
    }

    #[test]
    fn lookup_hot_reloads_after_mtime_change() {
        use std::fs;
        use std::thread::sleep;
        use std::time::Duration;
        let tmp = std::env::temp_dir().join(format!("nerv-hot-reload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("widget.json");
        fs::write(&path, r#"{"name":"widget","description":"v1"}"#).unwrap();

        let r = SpecRegistry::at_dir(&tmp);
        let v1 = r.lookup("widget").expect("v1");
        assert_eq!(v1.description.as_deref(), Some("v1"));

        // Bump mtime by at least 1s (filesystem coarse-grained on some
        // systems) and rewrite with v2 content.
        sleep(Duration::from_secs(1));
        fs::write(&path, r#"{"name":"widget","description":"v2"}"#).unwrap();

        let v2 = r.lookup("widget").expect("v2");
        assert_eq!(v2.description.as_deref(), Some("v2"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn lazy_lookup_reads_from_disk_on_miss() {
        let dir = workspace_fixture_specs_dir();
        let r = SpecRegistry::at_dir(&dir);
        assert!(r.is_empty(), "registry should start empty (lazy)");
        // First lookup reads from disk + caches.
        let git = r.lookup("git").expect("git fixture should load");
        assert_eq!(git.name, "git");
        assert!(!r.is_empty(), "cache should populate after lookup");
        // Negative cache: missing binary stays missing without retry.
        assert!(r.lookup("nonexistent-binary").is_none());
        assert!(r.lookup("nonexistent-binary").is_none()); // 2nd hit ok too
    }

    #[test]
    fn current_prefix_trailing_space_is_empty() {
        assert_eq!(current_prefix("git "), "");
        assert_eq!(current_prefix("git st"), "st");
        assert_eq!(current_prefix("git checkout ma"), "ma");
        assert_eq!(current_prefix(""), "");
    }

    #[test]
    fn tokenize_collapses_spaces() {
        let toks = tokenize("git  status");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].text, "git");
        assert_eq!(toks[1].text, "status");
    }

    // Both env-mutating tests below share HOME/HISTFILE in the same
    // process. Serialize them so parallel runs don't race.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn ssh_hosts_parses_known_hosts_and_config() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ssh_dir = tmp.path().join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::write(
            ssh_dir.join("known_hosts"),
            "# comment\n\
             github.com,140.82.112.4 ssh-rsa AAAA...\n\
             [example.com]:2222 ssh-ed25519 AAAA...\n\
             |1|salt|hash ssh-rsa AAAA...\n",
        )
        .unwrap();
        std::fs::write(
            ssh_dir.join("config"),
            "Host alpha\n  HostName 10.0.0.1\nHost beta gamma\n  User x\nHost *\nInclude inc.conf\n",
        )
        .unwrap();
        std::fs::write(ssh_dir.join("inc.conf"), "Host included\n  Port 22\n").unwrap();

        let home = tmp.path().to_path_buf();
        // SAFETY: single-threaded test; sets HOME for the helper.
        unsafe { std::env::set_var("HOME", &home) };
        let hosts = ssh_hosts().unwrap();
        assert!(hosts.contains(&"github.com".to_string()));
        assert!(hosts.contains(&"example.com".to_string()));
        assert!(hosts.contains(&"alpha".to_string()));
        assert!(hosts.contains(&"beta".to_string()));
        assert!(hosts.contains(&"gamma".to_string()));
        assert!(hosts.contains(&"included".to_string()));
        assert!(!hosts.iter().any(|h| h.contains('*')));
    }

    #[test]
    fn history_entries_handles_nonutf8_zsh_extended_format() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let hist = tmp.path().join(".zsh_history");
        // EXTENDED_HISTORY prefix + invalid UTF-8 bytes mixed in.
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b": 1700000000:0;curl https://example.com\n");
        bytes.extend_from_slice(b": 1700000001:0;git log\n");
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']); // bad utf-8 line
        bytes.extend_from_slice(b": 1700000002:0;ssh root@gamma\n");
        std::fs::write(&hist, bytes).unwrap();

        unsafe {
            std::env::set_var("HISTFILE", &hist);
            std::env::remove_var("ZDOTDIR");
        }
        let entries = shell_history_entries().unwrap();
        assert!(entries.iter().any(|s| s == "https://example.com"));
        assert!(entries.iter().any(|s| s == "git"));
        assert!(entries.iter().any(|s| s == "ssh"));
    }

    #[test]
    fn extract_enum_simple_pipe_list() {
        let got = extract_enum_from_description("Filter by state: {open|closed|merged|all}");
        assert_eq!(
            got,
            Some(vec![
                "open".into(),
                "closed".into(),
                "merged".into(),
                "all".into(),
            ])
        );
    }

    #[test]
    fn extract_enum_with_spaces_around_pipes() {
        let got = extract_enum_from_description("Mode: {auto | always | never}");
        assert_eq!(
            got,
            Some(vec!["auto".into(), "always".into(), "never".into()])
        );
    }

    #[test]
    fn extract_enum_ignores_single_value_braces() {
        // `{foo}` is not an enum — likely a placeholder.
        assert_eq!(extract_enum_from_description("Path: {file}"), None);
    }

    #[test]
    fn extract_enum_ignores_braces_with_disallowed_chars() {
        // `{a b|c d}` has spaces inside entries — not an enum list.
        assert_eq!(extract_enum_from_description("usage: {a b|c d}"), None);
    }

    #[test]
    fn extract_enum_no_braces_returns_none() {
        assert_eq!(extract_enum_from_description("just a description"), None);
    }

    #[test]
    fn human_size_small_bytes() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1023), "1023B");
    }

    #[test]
    fn human_size_kilobytes_use_one_decimal_under_100() {
        assert_eq!(human_size(1024), "1.00K");
        assert_eq!(human_size(2048), "2.00K");
        assert_eq!(human_size(15 * 1024), "15.0K");
    }

    #[test]
    fn human_size_at_unit_boundary_uses_no_decimals_above_100() {
        assert_eq!(human_size(150 * 1024), "150K");
        assert_eq!(human_size(5 * 1024 * 1024), "5.00M");
    }

    #[test]
    fn human_size_giga_scale() {
        assert_eq!(human_size(2 * 1024 * 1024 * 1024), "2.00G");
    }

    #[test]
    fn infer_filepaths_kind_file_names() {
        assert_eq!(infer_filepaths_kind(Some("path")), Some(false));
        assert_eq!(infer_filepaths_kind(Some("File")), Some(false));
        assert_eq!(infer_filepaths_kind(Some(" filename ")), Some(false));
    }

    #[test]
    fn infer_filepaths_kind_folder_names() {
        assert_eq!(infer_filepaths_kind(Some("dir")), Some(true));
        assert_eq!(infer_filepaths_kind(Some("DIRECTORY")), Some(true));
        assert_eq!(infer_filepaths_kind(Some("folder")), Some(true));
    }

    #[test]
    fn infer_filepaths_kind_unknown_returns_none() {
        assert_eq!(infer_filepaths_kind(Some("image")), None);
        assert_eq!(infer_filepaths_kind(Some("filename for output")), None);
        assert_eq!(infer_filepaths_kind(None), None);
    }

    #[test]
    fn infer_from_opt_long_file_name() {
        let opt = Opt {
            names: vec!["-f".into(), "--file".into()],
            ..Default::default()
        };
        assert_eq!(infer_filepaths_kind_from_opt_names(Some(&opt)), Some(false));
    }

    #[test]
    fn infer_from_opt_long_directory_name() {
        let opt = Opt {
            names: vec!["--directory".into()],
            ..Default::default()
        };
        assert_eq!(infer_filepaths_kind_from_opt_names(Some(&opt)), Some(true));
    }

    #[test]
    fn infer_from_opt_short_d_is_ambiguous_so_none() {
        let opt = Opt {
            names: vec!["-d".into()],
            ..Default::default()
        };
        assert_eq!(infer_filepaths_kind_from_opt_names(Some(&opt)), None);
    }

    fn sug(name: &str, prio: Option<u32>) -> Suggestion {
        Suggestion {
            insertion: name.into(),
            display: name.into(),
            description: None,
            kind: SuggestionKind::Argument,
            priority: prio,
            icon: None,
        }
    }

    #[test]
    fn priority_sort_higher_first() {
        let mut v = vec![sug("a", Some(50)), sug("b", Some(75)), sug("c", Some(25))];
        v.sort_by(sort_by_priority_then_alpha);
        assert_eq!(v[0].display, "b");
        assert_eq!(v[1].display, "a");
        assert_eq!(v[2].display, "c");
    }

    #[test]
    fn priority_sort_default_50_ties_break_alpha() {
        let mut v = [sug("zeta", None), sug("alpha", None)];
        v.sort_by(sort_by_priority_then_alpha);
        assert_eq!(v[0].display, "alpha");
        assert_eq!(v[1].display, "zeta");
    }

    #[test]
    fn priority_sort_explicit_50_equals_none() {
        let mut v = [sug("a", None), sug("b", Some(50))];
        v.sort_by(sort_by_priority_then_alpha);
        // Same priority → alpha sort wins
        assert_eq!(v[0].display, "a");
        assert_eq!(v[1].display, "b");
    }

    #[test]
    fn infer_from_opt_short_f_alone_treats_as_file() {
        let opt = Opt {
            names: vec!["-f".into()],
            ..Default::default()
        };
        assert_eq!(infer_filepaths_kind_from_opt_names(Some(&opt)), Some(false));
    }

    #[test]
    fn emit_appends_eq_for_requires_separator_with_args() {
        let node = Subcommand {
            name: "ls".into(),
            options: vec![Opt {
                names: vec!["--color".into()],
                requires_separator: true,
                args: vec![Arg::default()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let out = emit_options_with_ancestors(&node, &[], "--c", MatchMode::Prefix);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].insertion, "--color=");
        assert_eq!(out[0].display, "--color", "display stays clean");
    }

    #[test]
    fn emit_does_not_append_eq_when_no_args() {
        // requires_separator on a flag-only option is meaningless;
        // make sure we don't tack on `=` when there's nothing after.
        let node = Subcommand {
            name: "ls".into(),
            options: vec![Opt {
                names: vec!["--flag".into()],
                requires_separator: true,
                args: vec![],
                ..Default::default()
            }],
            ..Default::default()
        };
        let out = emit_options_with_ancestors(&node, &[], "--f", MatchMode::Prefix);
        assert_eq!(out[0].insertion, "--flag");
    }

    #[test]
    fn emit_no_eq_when_separator_not_required() {
        let node = Subcommand {
            name: "ls".into(),
            options: vec![Opt {
                names: vec!["--color".into()],
                requires_separator: false,
                args: vec![Arg::default()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let out = emit_options_with_ancestors(&node, &[], "--c", MatchMode::Prefix);
        assert_eq!(out[0].insertion, "--color");
    }

    #[test]
    fn sanitize_icon_strips_fig_urls() {
        assert_eq!(sanitize_icon(Some("fig://icon?type=string")), None);
        assert_eq!(sanitize_icon(Some("fig://template?color=red")), None);
    }

    #[test]
    fn sanitize_icon_keeps_emoji_and_short_glyph() {
        assert_eq!(sanitize_icon(Some("📦")), Some("📦".into()));
        assert_eq!(sanitize_icon(Some(">")), Some(">".into()));
    }

    #[test]
    fn sanitize_icon_drops_empty_and_long() {
        assert_eq!(sanitize_icon(None), None);
        assert_eq!(sanitize_icon(Some("")), None);
        assert_eq!(sanitize_icon(Some("   ")), None);
        assert_eq!(sanitize_icon(Some("toolong")), None);
    }

    #[test]
    fn sanitize_icon_rejects_one_cell_nonascii() {
        // Widget reserves 2 cells for any non-ASCII glyph; chars that
        // render in 1 cell on a default terminal (Latin-extended,
        // ambiguous-width per UAX #11) would shift the row by 1.
        // ⚠ (U+26A0, no VS-16) and à (U+00E0) are width 1.
        assert_eq!(sanitize_icon(Some("\u{26A0}")), None);
        assert_eq!(sanitize_icon(Some("\u{00E0}")), None);
    }

    #[test]
    fn sanitize_icon_keeps_two_cell_glyphs() {
        // CJK ideograph (3-byte) and supplementary emoji (4-byte) are
        // both width 2 — the canonical "wide" slot.
        assert_eq!(sanitize_icon(Some("\u{4E2D}")), Some("\u{4E2D}".into())); // 中
        assert_eq!(sanitize_icon(Some("📦")), Some("📦".into()));
        assert_eq!(sanitize_icon(Some("📝")), Some("📝".into()));
    }

    #[test]
    fn sanitize_icon_rejects_multichar_ascii() {
        // Width 2 ASCII string like "ok" would have been allowed by the
        // old byte-length check (2 ≤ 4) and the widget would still draw
        // one slot, mangling alignment. Force single ASCII char.
        assert_eq!(sanitize_icon(Some("ok")), None);
        assert_eq!(sanitize_icon(Some(">>")), None);
    }

    #[test]
    fn subcommand_icon_propagates_to_suggestion() {
        let node = Subcommand {
            name: "git".into(),
            subcommands: vec![Subcommand {
                name: "commit".into(),
                icon: Some("📝".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let out = emit_subcommands(&node, "com", MatchMode::Prefix);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].icon.as_deref(), Some("📝"));
    }

    #[test]
    fn emit_subcommands_fuzzy_mode_matches_subsequence() {
        let node = Subcommand {
            name: "git".into(),
            subcommands: vec![
                Subcommand {
                    name: "commit".into(),
                    ..Default::default()
                },
                Subcommand {
                    name: "checkout".into(),
                    ..Default::default()
                },
                Subcommand {
                    name: "config".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // Prefix mode: "co" matches commit + config (not checkout).
        let prefix = emit_subcommands(&node, "co", MatchMode::Prefix);
        let prefix_names: Vec<&str> = prefix.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(prefix_names, vec!["commit", "config"]);

        // Fuzzy mode: "cmt" matches commit (c-o-m-m-i-T → c-m-t).
        let fuzzy = emit_subcommands(&node, "cmt", MatchMode::Fuzzy);
        assert_eq!(fuzzy.len(), 1);
        assert_eq!(fuzzy[0].display, "commit");

        // Fuzzy mode: "ck" matches checkout but NOT commit / config.
        let fuzzy_ck = emit_subcommands(&node, "ck", MatchMode::Fuzzy);
        assert_eq!(fuzzy_ck.len(), 1);
        assert_eq!(fuzzy_ck[0].display, "checkout");
    }

    #[test]
    fn emit_subcommands_fuzzy_mode_matches_aliases() {
        let node = Subcommand {
            name: "git".into(),
            subcommands: vec![Subcommand {
                name: "checkout".into(),
                aliases: vec!["co".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        // Prefix "co" hits the alias.
        let prefix = emit_subcommands(&node, "co", MatchMode::Prefix);
        assert_eq!(prefix.len(), 1);
        // Fuzzy "ck" hits the canonical name (alias is shorter than query).
        let fuzzy = emit_subcommands(&node, "ck", MatchMode::Fuzzy);
        assert_eq!(fuzzy.len(), 1);
    }

    #[test]
    fn fuzzy_subsequence_match_basics() {
        assert!(fuzzy_subsequence_match("checkout", "chk"));
        assert!(fuzzy_subsequence_match("checkout", ""));
        assert!(fuzzy_subsequence_match("checkout", "checkout"));
        assert!(!fuzzy_subsequence_match("checkout", "checkouts"));
        assert!(!fuzzy_subsequence_match("checkout", "xyz"));
        // Out-of-order: query 't' before 'c' in name → fail.
        assert!(!fuzzy_subsequence_match("checkout", "tc"));
    }

    #[test]
    fn split_by_query_term_no_delim_returns_whole_prefix() {
        let (q, ip) = split_by_query_term("tokio,serde", None);
        assert_eq!(q, "tokio,serde");
        assert_eq!(ip, "");
    }

    #[test]
    fn split_by_query_term_after_last_delim() {
        let (q, ip) = split_by_query_term("tokio,serde,async", Some(","));
        assert_eq!(q, "async");
        assert_eq!(ip, "tokio,serde,");
    }

    #[test]
    fn split_by_query_term_multi_delim_set() {
        // delim set "@,": last `@` wins for `pkg@1.0,foo@`
        let (q, ip) = split_by_query_term("pkg@1.0,foo@", Some("@,"));
        assert_eq!(q, "");
        assert_eq!(ip, "pkg@1.0,foo@");
    }

    #[test]
    fn split_by_query_term_no_match_returns_whole() {
        let (q, ip) = split_by_query_term("tokio", Some(","));
        assert_eq!(q, "tokio");
        assert_eq!(ip, "");
    }

    #[test]
    fn arg_with_get_query_term_preserves_prefix_in_insertion() {
        use crate::spec_parser::{Arg, RawSuggestion};
        let arg = Arg {
            get_query_term: Some(",".into()),
            suggestions: vec![
                RawSuggestion {
                    name: "serde".into(),
                    ..Default::default()
                },
                RawSuggestion {
                    name: "async-trait".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let out = emit_candidates_for_arg(&arg, "tokio,se", None, None, MatchMode::Prefix, &[]);
        // Only "serde" matches "se" prefix.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].display, "serde");
        // Insertion preserves the pre-comma context.
        assert_eq!(out[0].insertion, "tokio,serde");
    }

    #[test]
    fn dir_summary_empty_returns_empty_label() {
        let tmp = std::env::temp_dir().join(format!("nerv-test-empty-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(dir_summary(&tmp), "empty");
        let _ = std::fs::remove_dir(&tmp);
    }

    #[test]
    fn dir_summary_skips_dotfiles_and_pluralizes() {
        let tmp = std::env::temp_dir().join(format!("nerv-test-count-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "x").unwrap();
        std::fs::write(tmp.join("b.txt"), "x").unwrap();
        std::fs::write(tmp.join(".hidden"), "x").unwrap();
        assert_eq!(dir_summary(&tmp), "2 items");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dir_summary_singular_for_one_item() {
        let tmp = std::env::temp_dir().join(format!("nerv-test-one-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("only.txt"), "x").unwrap();
        assert_eq!(dir_summary(&tmp), "1 item");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn matches_filter_prefix_is_default() {
        assert!(matches_filter("foobar", "foo", None, MatchMode::Prefix));
        assert!(!matches_filter("foobar", "bar", None, MatchMode::Prefix));
    }

    #[test]
    fn matches_filter_substring_matches_anywhere() {
        assert!(matches_filter(
            "foobar",
            "oob",
            Some("substring"),
            MatchMode::Prefix
        ));
        assert!(matches_filter(
            "foobar",
            "bar",
            Some("substring"),
            MatchMode::Prefix
        ));
        assert!(matches_filter(
            "foobar",
            "foo",
            Some("substring"),
            MatchMode::Prefix
        ));
        assert!(!matches_filter(
            "foobar",
            "xyz",
            Some("substring"),
            MatchMode::Prefix
        ));
    }

    #[test]
    fn matches_filter_fuzzy_strategy_is_prefix_under_prefix_mode() {
        // Spec declares `filterStrategy: "fuzzy"` but user runs default
        // prefix mode → behaves like prefix (no surprise subsequence).
        assert!(matches_filter(
            "foobar",
            "foo",
            Some("fuzzy"),
            MatchMode::Prefix
        ));
        assert!(!matches_filter(
            "foobar",
            "bar",
            Some("fuzzy"),
            MatchMode::Prefix
        ));
        assert!(!matches_filter(
            "foobar",
            "fbr",
            Some("fuzzy"),
            MatchMode::Prefix
        ));
    }

    #[test]
    fn matches_filter_fuzzy_mode_does_subsequence() {
        // User opts in via `[matching] mode = "fuzzy"`.
        assert!(matches_filter("checkout", "chk", None, MatchMode::Fuzzy));
        assert!(matches_filter("commit", "cmt", None, MatchMode::Fuzzy));
        assert!(matches_filter("foobar", "fbr", None, MatchMode::Fuzzy));
        // Out-of-order chars still fail.
        assert!(!matches_filter("foobar", "rba", None, MatchMode::Fuzzy));
    }

    #[test]
    fn matches_filter_substring_strategy_overrides_fuzzy_mode() {
        // Per-arg `filterStrategy: "substring"` is a strong override —
        // it must beat user fuzzy mode so spec authors keep control.
        assert!(matches_filter(
            "foobar",
            "oob",
            Some("substring"),
            MatchMode::Fuzzy
        ));
        assert!(!matches_filter(
            "foobar",
            "xyz",
            Some("substring"),
            MatchMode::Fuzzy
        ));
    }

    #[test]
    fn matches_filter_fuzzy_mode_is_case_insensitive() {
        assert!(matches_filter("Checkout", "chk", None, MatchMode::Fuzzy));
        assert!(matches_filter("checkout", "CHK", None, MatchMode::Fuzzy));
    }

    #[test]
    fn substring_filter_works_on_arg_suggestions() {
        use crate::spec_parser::{Arg, RawSuggestion};
        let arg = Arg {
            filter_strategy: Some("substring".into()),
            suggestions: vec![
                RawSuggestion {
                    name: "foobar".into(),
                    ..Default::default()
                },
                RawSuggestion {
                    name: "barfoo".into(),
                    ..Default::default()
                },
                RawSuggestion {
                    name: "xyz".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let out = emit_candidates_for_arg(&arg, "foo", None, None, MatchMode::Prefix, &[]);
        // Both "foobar" and "barfoo" contain "foo".
        let names: Vec<_> = out.iter().map(|s| s.display.as_str()).collect();
        assert!(names.contains(&"foobar"));
        assert!(names.contains(&"barfoo"));
        assert!(!names.contains(&"xyz"));
    }

    #[test]
    fn opt_icon_strips_fig_url() {
        let node = Subcommand {
            name: "git".into(),
            options: vec![Opt {
                names: vec!["--all".into()],
                icon: Some("fig://icon?type=command".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let out = emit_options_with_ancestors(&node, &[], "--a", MatchMode::Prefix);
        assert_eq!(out[0].icon, None, "fig:// URL must not leak");
    }

    // --- Edge case coverage (tech-debt sweep) ---

    #[test]
    fn sanitize_icon_at_exact_4_byte_boundary() {
        // Common 4-byte emoji should pass.
        assert_eq!(sanitize_icon(Some("📦")), Some("📦".into()));
        assert_eq!(sanitize_icon(Some("📁")), Some("📁".into()));
        // 5+ bytes (emoji + ASCII) must not.
        assert_eq!(sanitize_icon(Some("📦x")), None);
        // 3-byte CJK char passes (≤4).
        assert_eq!(sanitize_icon(Some("文")), Some("文".into()));
    }

    #[test]
    fn sanitize_icon_trims_whitespace() {
        assert_eq!(sanitize_icon(Some("  📦  ")), Some("📦".into()));
        assert_eq!(sanitize_icon(Some("\t$\n")), Some("$".into()));
    }

    #[test]
    fn split_by_query_term_handles_multibyte_delim() {
        // CJK delim char (3 bytes). Make sure byte-position split
        // doesn't fall inside a char boundary.
        let (q, ip) = split_by_query_term("foo,bar", Some(","));
        assert_eq!(q, "bar");
        assert_eq!(ip, "foo,");
        // Empty prefix.
        let (q, ip) = split_by_query_term("", Some(","));
        assert_eq!(q, "");
        assert_eq!(ip, "");
        // Trailing delim → empty query, full insert prefix.
        let (q, ip) = split_by_query_term("foo,", Some(","));
        assert_eq!(q, "");
        assert_eq!(ip, "foo,");
    }

    #[test]
    fn split_by_query_term_utf8_safe_with_emoji() {
        // Emoji before delim — split must land at char boundary.
        let (q, ip) = split_by_query_term("📦,bar", Some(","));
        assert_eq!(q, "bar");
        assert_eq!(ip, "📦,");
    }

    #[test]
    fn split_by_query_term_empty_delim_string_returns_whole() {
        let (q, ip) = split_by_query_term("foo,bar", Some(""));
        assert_eq!(q, "foo,bar");
        assert_eq!(ip, "");
    }

    #[test]
    fn matches_filter_empty_query_matches_anything() {
        assert!(matches_filter("foobar", "", None, MatchMode::Prefix));
        assert!(matches_filter(
            "foobar",
            "",
            Some("substring"),
            MatchMode::Prefix
        ));
        assert!(matches_filter("", "", None, MatchMode::Prefix));
        // Fuzzy mode preserves the same property.
        assert!(matches_filter("foobar", "", None, MatchMode::Fuzzy));
    }

    #[test]
    fn matches_filter_substring_empty_string_in_empty_name() {
        assert!(matches_filter("", "", Some("substring"), MatchMode::Prefix));
        assert!(!matches_filter(
            "",
            "foo",
            Some("substring"),
            MatchMode::Prefix
        ));
    }

    #[test]
    fn extract_aws_json_names_array_with_id_field() {
        // The canonical aws shape: parent_key resolves to an array of
        // objects, each carrying an `Arn` (or similar) ID field.
        let raw = r#"{"OpenIDConnectProviderList":[{"Arn":"arn:aws:iam::1:oidc/A"},{"Arn":"arn:aws:iam::1:oidc/B"}]}"#;
        let out = extract_aws_json_names(raw, "OpenIDConnectProviderList", Some("Arn")).unwrap();
        assert_eq!(out, vec!["arn:aws:iam::1:oidc/A", "arn:aws:iam::1:oidc/B"]);
    }

    #[test]
    fn extract_aws_json_names_array_without_id_field() {
        // When `id_field` is None the upstream closure emits each
        // element as a bare scalar — mirror that.
        let raw = r#"{"Buckets":["a","b","c"]}"#;
        let out = extract_aws_json_names(raw, "Buckets", None).unwrap();
        assert_eq!(out, vec!["a", "b", "c"]);
    }

    #[test]
    fn extract_aws_json_names_single_object_with_id_field() {
        // The non-array branch — Fig's helper folds a single object
        // into a one-item list when parent_key is not an array.
        let raw = r#"{"Function":{"Arn":"arn:aws:lambda::1:f/MyFn"}}"#;
        let out = extract_aws_json_names(raw, "Function", Some("Arn")).unwrap();
        assert_eq!(out, vec!["arn:aws:lambda::1:f/MyFn"]);
    }

    #[test]
    fn extract_aws_json_names_missing_parent_returns_none() {
        let raw = r#"{"Other":[]}"#;
        assert!(extract_aws_json_names(raw, "Buckets", None).is_none());
    }

    #[test]
    fn extract_aws_json_names_invalid_json_returns_none() {
        assert!(extract_aws_json_names("not json", "Any", None).is_none());
    }

    #[test]
    fn extract_aws_json_names_skips_elements_missing_id_field() {
        // Defensive: an aws CLI response with sparse objects shouldn't
        // panic; missing fields drop the entry.
        let raw = r#"{"Items":[{"Id":"a"},{"NoId":"x"},{"Id":"b"}]}"#;
        let out = extract_aws_json_names(raw, "Items", Some("Id")).unwrap();
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn spec_stem_from_path_strips_extensions() {
        assert_eq!(
            spec_stem_from_path(Path::new("/tmp/specs/git.json")),
            Some("git".to_string())
        );
        assert_eq!(
            spec_stem_from_path(Path::new("/tmp/specs/docker.json.gz")),
            Some("docker".to_string())
        );
        assert!(spec_stem_from_path(Path::new("/tmp/specs/.swap")).is_none());
        assert!(spec_stem_from_path(Path::new("/tmp/specs/git.json.bak")).is_none());
    }

    #[test]
    fn fs_watcher_invalidates_cache_on_spec_change() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_path = tmp.path().join("foo.json");
        std::fs::write(
            &spec_path,
            r#"{"name":"foo","subcommands":[{"name":"alpha"}]}"#,
        )
        .unwrap();
        let reg = SpecRegistry::at_dir(tmp.path());
        let first = reg.lookup("foo").expect("initial load failed");
        assert_eq!(first.subcommands.len(), 1);
        assert_eq!(first.subcommands[0].name, "alpha");

        // Rewrite the spec. The FS watcher should mark `foo` dirty so
        // the next lookup re-reads from disk with the new subcommand
        // list. macOS FSEvents coalesces at 500ms — wait long enough.
        std::fs::write(
            &spec_path,
            r#"{"name":"foo","subcommands":[{"name":"alpha"},{"name":"beta"}]}"#,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut second = reg.lookup("foo").unwrap();
        while second.subcommands.len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
            second = reg.lookup("foo").unwrap();
        }
        assert_eq!(second.subcommands.len(), 2);
    }

    /// Poll `lookup(name)` until the closure says "good enough" or the
    /// deadline expires. macOS FSEvents coalesces events at ~500ms so
    /// a single immediate read after writing a file is not enough.
    fn wait_for_lookup<F>(
        reg: &SpecRegistry,
        name: &str,
        deadline: std::time::Duration,
        ok: F,
    ) -> Option<std::sync::Arc<Spec>>
    where
        F: Fn(&Option<std::sync::Arc<Spec>>) -> bool,
    {
        let stop = std::time::Instant::now() + deadline;
        loop {
            let got = reg.lookup(name);
            if ok(&got) {
                return got;
            }
            if std::time::Instant::now() >= stop {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn fs_watcher_invalidates_on_spec_removal() {
        // A spec file disappearing from the cache dir (e.g. `build-specs`
        // running with a smaller --only filter) must drop the in-memory
        // entry so subsequent lookups miss. Watcher Remove events feed
        // the same `pending` set as Modify, so the path under test is
        // the same drain → cache.remove → load_from_disk → None flow.
        let tmp = tempfile::tempdir().unwrap();
        let spec_path = tmp.path().join("ephemeral.json");
        std::fs::write(&spec_path, r#"{"name":"ephemeral"}"#).unwrap();

        let reg = SpecRegistry::at_dir(tmp.path());
        let first = reg.lookup("ephemeral").expect("initial load failed");
        assert_eq!(first.name, "ephemeral");

        std::fs::remove_file(&spec_path).unwrap();
        let after = wait_for_lookup(
            &reg,
            "ephemeral",
            std::time::Duration::from_secs(3),
            |got| got.is_none(),
        );
        assert!(after.is_none(), "expected None after removal");
    }

    #[test]
    fn fs_watcher_picks_up_late_arriving_spec() {
        // Lazy SpecRegistry: a lookup that misses populates a negative
        // cache entry. When the user runs `build-specs` mid-session and
        // a brand-new file appears, the FS watcher's Create event must
        // evict the negative entry so the very next lookup re-reads
        // from disk and finds the new spec.
        let tmp = tempfile::tempdir().unwrap();
        let reg = SpecRegistry::at_dir(tmp.path());
        assert!(reg.lookup("late").is_none(), "dir starts empty");

        let spec_path = tmp.path().join("late.json");
        std::fs::write(&spec_path, r#"{"name":"late"}"#).unwrap();

        let after = wait_for_lookup(&reg, "late", std::time::Duration::from_secs(3), |got| {
            got.is_some()
        });
        assert!(after.is_some(), "expected late.json to be picked up");
        assert_eq!(after.unwrap().name, "late");
    }

    #[test]
    fn fs_watcher_reloads_gzipped_spec_files() {
        // Production caches ship as `*.json.gz` (10× smaller). The
        // watcher's stem extractor strips `.json.gz` the same as `.json`
        // and `load_from_disk` falls back to the gz path when plain
        // doesn't exist. End-to-end: rewriting a `.json.gz` should
        // re-render the cached spec on the next lookup.
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let spec_path = tmp.path().join("gz_spec.json.gz");
        let write_gz = |path: &std::path::Path, body: &str| {
            let f = std::fs::File::create(path).unwrap();
            let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            enc.write_all(body.as_bytes()).unwrap();
            enc.finish().unwrap();
        };
        write_gz(
            &spec_path,
            r#"{"name":"gz_spec","subcommands":[{"name":"v1"}]}"#,
        );

        let reg = SpecRegistry::at_dir(tmp.path());
        let first = reg.lookup("gz_spec").expect("initial gz load failed");
        assert_eq!(first.subcommands[0].name, "v1");

        write_gz(
            &spec_path,
            r#"{"name":"gz_spec","subcommands":[{"name":"v2"}]}"#,
        );
        let second = wait_for_lookup(&reg, "gz_spec", std::time::Duration::from_secs(3), |got| {
            got.as_ref()
                .is_some_and(|s| s.subcommands.first().is_some_and(|c| c.name == "v2"))
        })
        .expect("expected reload");
        assert_eq!(second.subcommands[0].name, "v2");
    }

    #[test]
    fn drain_invalidations_is_noop_without_watcher() {
        // SpecRegistry::default / empty has no watcher → no pending
        // queue. drain_invalidations must early-return without touching
        // the cache. Verified indirectly: lookup against an in-memory
        // inserted spec still returns it after a drain pass.
        let reg = SpecRegistry::empty();
        let spec = Spec {
            name: "in_memory".into(),
            ..Default::default()
        };
        reg.insert(spec);
        // Force drain via lookup — must NOT evict our entry.
        let got = reg
            .lookup("in_memory")
            .expect("in-memory entry must survive");
        assert_eq!(got.name, "in_memory");
    }

    #[test]
    fn build_aws_list_command_no_flags() {
        let cmd = build_aws_list_command("ec2", "describe-instances", &[], &[]);
        assert_eq!(cmd, vec!["aws", "ec2", "describe-instances"]);
    }

    #[test]
    fn build_aws_list_command_with_matching_flag() {
        let tokens = vec![
            Annotation {
                text: "lambda".into(),
                span: 0..6,
                kind: TokenKind::Unknown,
            },
            Annotation {
                text: "list-layer-versions".into(),
                span: 7..26,
                kind: TokenKind::Unknown,
            },
            Annotation {
                text: "--layer-name".into(),
                span: 27..39,
                kind: TokenKind::Unknown,
            },
            Annotation {
                text: "MyLayer".into(),
                span: 40..47,
                kind: TokenKind::Unknown,
            },
        ];
        let cmd = build_aws_list_command(
            "lambda",
            "list-layer-versions",
            &["--layer-name".into()],
            &tokens,
        );
        assert_eq!(
            cmd,
            vec![
                "aws",
                "lambda",
                "list-layer-versions",
                "--layer-name",
                "MyLayer"
            ]
        );
    }

    #[test]
    fn build_aws_list_command_skips_missing_flags() {
        // Token list lacks `--region` — the flag is silently dropped
        // (mirrors the upstream `tokens.indexOf` continue branch).
        let tokens = vec![Annotation {
            text: "ecs".into(),
            span: 0..3,
            kind: TokenKind::Unknown,
        }];
        let cmd = build_aws_list_command("ecs", "list-clusters", &["--region".into()], &tokens);
        assert_eq!(cmd, vec!["aws", "ecs", "list-clusters"]);
    }

    #[test]
    fn build_aws_list_command_drops_trailing_flag_without_value() {
        // The flag is the last token — no value after it. Drop the
        // flag rather than emit a half-baked command.
        let tokens = vec![Annotation {
            text: "--layer-name".into(),
            span: 0..12,
            kind: TokenKind::Unknown,
        }];
        let cmd = build_aws_list_command(
            "lambda",
            "list-layer-versions",
            &["--layer-name".into()],
            &tokens,
        );
        assert_eq!(cmd, vec!["aws", "lambda", "list-layer-versions"]);
    }

    #[test]
    fn dir_summary_handles_unreadable_dir() {
        // Nonexistent path → fallback "dir".
        let p = std::path::PathBuf::from("/this/does/not/exist/anywhere");
        assert_eq!(dir_summary(&p), "dir");
    }

    #[test]
    fn dir_summary_truncates_at_cap() {
        // Create a dir with > 200 entries; should report `n+ items`.
        let tmp = std::env::temp_dir().join(format!("nerv-cap-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        for i in 0..210u32 {
            std::fs::write(tmp.join(format!("f{i}")), "x").unwrap();
        }
        let s = dir_summary(&tmp);
        assert!(s.ends_with("+ items"), "expected truncated label: {s}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Tier C closure dispatch — only compiled under `--features
    /// quickjs`. Default builds skip these (the arm itself is feature-
    /// gated; without it the `Custom` variant falls through to the
    /// catch-all and emits zero candidates, which the
    /// `custom_without_feature_falls_through` test below verifies).
    #[cfg(feature = "quickjs")]
    mod tier_c_dispatch {
        use super::*;
        use crate::spec_parser::{Arg, Generator, Subcommand};

        fn spec_with_custom(source: &str) -> Subcommand {
            Subcommand {
                name: "x".into(),
                args: vec![Arg {
                    name: Some("opt".into()),
                    generators: vec![Generator::Custom {
                        description_hint: None,
                        source: Some(source.into()),
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }
        }

        #[test]
        fn closure_returns_string_array() {
            // Engine post-sorts emitted candidates alphabetically;
            // assert on the sorted set rather than insertion order.
            let src = r#"(() => ["main", "dev", "feature/x"])()"#;
            let r = complete("x ", 2, &registry_with(spec_with_custom(src)));
            let mut names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
            names.sort();
            assert_eq!(names, ["dev", "feature/x", "main"]);
        }

        #[test]
        fn closure_sees_tokens_via_global() {
            // Closures that capture the live token list reach it via
            // `globalThis.__nerv_tokens`. Lower-case each token so the
            // case-sensitive prefix gate still admits the result.
            let src = "(tokens => tokens.map(t => t.toLowerCase()))(globalThis.__nerv_tokens)";
            let r = complete("x gi", 4, &registry_with(spec_with_custom(src)));
            let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
            // Tokens are ["x", "gi"]; lower-cased → ["x", "gi"]. The
            // current-token prefix is "gi" → only "gi" survives the
            // matches_name (prefix) gate.
            assert_eq!(names, ["gi"]);
        }

        #[test]
        fn closure_throw_falls_through_silently() {
            let src = r#"(() => { throw new Error("boom") })()"#;
            let r = complete("x ", 2, &registry_with(spec_with_custom(src)));
            assert!(r.items.is_empty());
        }

        #[test]
        fn closure_with_no_source_is_skipped() {
            // Generator::Custom without `source` — the Tier C arm
            // doesn't match, so no candidates emit. The smart
            // filepaths fallback might fire if arg.name implies a
            // path; here arg name is "opt" so nothing kicks in.
            let spec = Subcommand {
                name: "x".into(),
                args: vec![Arg {
                    name: Some("opt".into()),
                    generators: vec![Generator::Custom {
                        description_hint: None,
                        source: None,
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };
            let r = complete("x ", 2, &registry_with(spec));
            assert!(r.items.is_empty());
        }

        #[test]
        fn closure_extracts_name_from_object_array() {
            // Fig closures often return `[{name, description}]` — the
            // tier_c extractor reads the `name` field.
            let src = r#"(() => [{ name: "alpha" }, { name: "beta" }])()"#;
            let r = complete("x ", 2, &registry_with(spec_with_custom(src)));
            let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
            assert_eq!(names, ["alpha", "beta"]);
        }
    }

    /// When the `quickjs` feature is OFF (default), a `Generator::Custom`
    /// with `source: Some(...)` still parses cleanly and emits no
    /// candidates. This locks in the wire-compat guarantee: converter
    /// JSON written by a quickjs-aware build is still loadable by the
    /// quickjs-free default binary.
    #[cfg(not(feature = "quickjs"))]
    #[test]
    fn custom_with_source_loads_in_default_build() {
        use crate::spec_parser::{Arg, Generator, Subcommand};
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("opt".into()),
                generators: vec![Generator::Custom {
                    description_hint: None,
                    source: Some(r#"(() => ["unused"])()"#.into()),
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = complete("x ", 2, &registry_with(spec));
        // Default build: no Tier C arm, no candidates. (smart
        // filepaths fallback ignores arg.name = "opt".)
        assert!(r.items.is_empty());
    }

    #[test]
    fn filepath_icons_assigned_per_kind() {
        // Symlinks > dirs > files in the icon precedence chain.
        let tmp = std::env::temp_dir().join(format!("nerv-fp-icons-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(tmp.join("subdir")).unwrap();
        std::fs::write(tmp.join("file.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.join("file.txt"), tmp.join("link")).unwrap();
        let rows = filepaths_at(Some(&tmp), "", false).unwrap();
        let by_name: std::collections::HashMap<_, _> = rows
            .into_iter()
            .map(|(_, display, _, icon)| (display, icon))
            .collect();
        assert_eq!(by_name.get("subdir/"), Some(&Some("📁".into())));
        assert_eq!(by_name.get("file.txt"), Some(&Some("📄".into())));
        #[cfg(unix)]
        assert_eq!(by_name.get("link"), Some(&Some("🔗".into())));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
