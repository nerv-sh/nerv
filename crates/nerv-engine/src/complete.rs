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
#[derive(Debug)]
pub struct SpecRegistry {
    dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, CacheEntry>>,
}

impl Default for SpecRegistry {
    fn default() -> Self {
        Self {
            dir: None,
            cache: RwLock::new(HashMap::new()),
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
    pub fn at_dir(dir: &Path) -> Self {
        Self {
            dir: Some(dir.to_path_buf()),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Build a registry rooted at `dir` and eagerly scan it for parse
    /// errors. Useful at daemon startup so problems show up in logs
    /// without waiting for a user keystroke. Returns the registry +
    /// every error encountered during the scan; positive results are
    /// kept in the cache so subsequent lookups are O(1).
    pub fn load_dir(dir: &Path) -> (Self, Vec<SpecLoadError>) {
        let registry = Self::at_dir(dir);
        let mut errors = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return (registry, errors);
        };
        let mut cache = registry.cache.write().expect("cache poisoned");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
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
    pub fn lookup(&self, name: &str) -> Option<Arc<Spec>> {
        // Fast path: cache hit + mtime unchanged.
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

/// Pipeline result: completion candidates at the cursor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompleteResult {
    /// Ranked candidate list (prefix-filtered).
    pub items: Vec<Suggestion>,
    /// Optional reason when items is empty (debug aid).
    pub reason: Option<String>,
}

/// Run the full pipeline against `line` + `cursor` byte offset.
pub fn complete(line: &str, cursor: usize, registry: &SpecRegistry) -> CompleteResult {
    complete_in(line, cursor, registry, None)
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
    let prefix_is_option = prefix.starts_with('-') && !current.options.is_empty();
    let prefer_subcommands = !prefix.starts_with('-')
        && !current.subcommands.is_empty()
        && matches!(
            result.cursor_context,
            CursorContext::Subcommand | CursorContext::Arg
        );

    let items = if prefix_is_option {
        emit_options(current, &prefix)
    } else if prefer_subcommands {
        // yarn-style shorthand: `yarn web` should match both yarn
        // subcommands (none start with "web") and the root args
        // generator (npmScriptsGenerator → web:start, web:build:dev,
        // …). Merge whenever the level has args with dynamic source.
        let mut subs = emit_subcommands(current, &prefix);
        if arg_has_dynamic_source(current) {
            subs.extend(emit_arg_candidates(current, &prefix, cwd));
        }
        subs.sort_by(|a, b| a.display.cmp(&b.display));
        subs.dedup_by(|a, b| a.display == b.display);
        subs
    } else {
        match result.cursor_context {
            CursorContext::Subcommand => emit_subcommands(current, &prefix),
            CursorContext::OptionName => emit_options(current, &prefix),
            CursorContext::Arg => emit_arg_candidates(current, &prefix, cwd),
            CursorContext::Done => vec![],
        }
    };

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

fn walk_to_current<'a>(root: &'a Spec, path: &[String]) -> Option<&'a Subcommand> {
    let mut node = root;
    for name in path.iter().skip(1) {
        node = find_subcommand(node, name)?;
    }
    Some(node)
}

fn emit_subcommands(node: &Subcommand, prefix: &str) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = node
        .subcommands
        .iter()
        .filter(|sc| !sc.hidden)
        .filter(|sc| name_or_aliases_match(&sc.name, &sc.aliases, prefix))
        .map(|sc| Suggestion {
            insertion: sc.name.clone(),
            display: sc.name.clone(),
            description: sc.description.clone(),
            kind: SuggestionKind::Subcommand,
        })
        .collect();
    out.sort_by(|a, b| a.display.cmp(&b.display));
    out
}

fn emit_options(node: &Subcommand, prefix: &str) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = node
        .options
        .iter()
        .filter(|o| !o.hidden)
        .flat_map(|o| {
            o.names
                .iter()
                .filter(|n| n.starts_with(prefix))
                .map(move |n| Suggestion {
                    insertion: n.clone(),
                    display: n.clone(),
                    description: o.description.clone(),
                    kind: SuggestionKind::Flag,
                })
        })
        .collect();
    out.sort_by(|a, b| a.display.cmp(&b.display));
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
) -> Vec<Suggestion> {
    let Some(arg) = node.args.first() else {
        return vec![];
    };

    // Static suggestions list (Tier A).
    let mut out: Vec<Suggestion> = arg
        .suggestions
        .iter()
        .filter(|s| s.starts_with(prefix))
        .map(|s| Suggestion {
            insertion: s.clone(),
            display: s.clone(),
            description: None,
            kind: SuggestionKind::Argument,
        })
        .collect();

    // Tier B: spawn `Generator::Template` scripts and parse stdout
    // lines as candidates. Skipped under NERV_NO_GENERATORS=1 (tests,
    // sandboxed environments).
    if std::env::var_os("NERV_NO_GENERATORS").is_none() {
        for g in &arg.generators {
            match g {
                crate::spec_parser::Generator::Template { script } => {
                    if let Some(lines) = cached_template_generator(script) {
                        out.extend(lines.into_iter().filter(|s| s.starts_with(prefix)).map(
                            |line| Suggestion {
                                insertion: line.clone(),
                                display: line,
                                description: None,
                                kind: SuggestionKind::Argument,
                            },
                        ));
                    }
                }
                crate::spec_parser::Generator::PackageJsonScripts => {
                    if let Some(scripts) = package_json_scripts(cwd) {
                        out.extend(
                            scripts
                                .into_iter()
                                .filter(|(name, _)| name.starts_with(prefix))
                                .map(|(name, cmd)| Suggestion {
                                    insertion: name.clone(),
                                    display: name,
                                    description: Some(cmd),
                                    kind: SuggestionKind::Argument,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::Filepaths { folders_only } => {
                    if let Some(paths) = filepaths_at(cwd, prefix, *folders_only) {
                        out.extend(paths.into_iter().map(|(insertion, display)| Suggestion {
                            insertion,
                            display,
                            description: None,
                            kind: SuggestionKind::Argument,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    out.sort_by(|a, b| a.display.cmp(&b.display));
    out.dedup_by(|a, b| a.display == b.display);
    out
}

type GeneratorCacheMap = HashMap<Vec<String>, (std::time::Instant, Vec<String>)>;

/// Process-wide cache for Tier B generator results.
/// Key: script argv. Value: (insertion-time, captured stdout lines).
/// TTL: 5s. Max entries: 64 (oldest-evicted on overflow). Keeps
/// per-keystroke completion calls from re-spawning the same shell
/// command (e.g. `git branch --list`).
static GENERATOR_CACHE: std::sync::LazyLock<std::sync::Mutex<GeneratorCacheMap>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

const GENERATOR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const GENERATOR_CACHE_MAX: usize = 64;

/// Cached wrapper around [`execute_template_generator`]. Returns
/// the cached value on TTL-fresh hit, otherwise runs the generator
/// and inserts the result.
fn cached_template_generator(script: &[String]) -> Option<Vec<String>> {
    let key = script.to_vec();
    if let Ok(cache) = GENERATOR_CACHE.lock() {
        if let Some((stamp, lines)) = cache.get(&key) {
            if stamp.elapsed() < GENERATOR_CACHE_TTL {
                return Some(lines.clone());
            }
        }
    }
    let lines = execute_template_generator(script)?;
    if let Ok(mut cache) = GENERATOR_CACHE.lock() {
        // Drop oldest entry when full (simple LRU stand-in; for 64
        // slots the O(n) scan is cheaper than dragging in a real LRU).
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
/// Hard-capped at 200ms wall time to keep the IPC roundtrip under the
/// 25ms p95 budget even on a busy machine — any longer means the
/// shell command itself is the bottleneck, not the engine.
fn execute_template_generator(script: &[String]) -> Option<Vec<String>> {
    use std::process::{Command, Stdio};
    use std::time::Duration;
    if script.is_empty() {
        return None;
    }
    let bin = script.first()?;
    let args = &script[1..];
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Polling wait — std::process::Child doesn't have async wait.
    // For 200ms total we sleep in 10ms increments (max 20 polls).
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = text
        .lines()
        .map(sanitize_generator_line)
        .filter(|s| !s.is_empty())
        .collect();
    Some(lines)
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
// Well-known generator: filepaths / folders (cd, cat, ls, …)
// ---------------------------------------------------------------------------

/// List directory entries matching the trailing-basename portion of
/// `prefix`. Honours `folders_only` (e.g. `cd` uses showFolders=only).
/// Returns `(insertion, display)` pairs — insertion preserves the
/// user's typed directory prefix so the widget's word-level replace
/// doesn't lose context (`cd ./fo<Tab>` → `cd ./encl/`, not `cd encl/`).
fn filepaths_at(
    cwd: Option<&std::path::Path>,
    prefix: &str,
    folders_only: bool,
) -> Option<Vec<(String, String)>> {
    // Split prefix into (dir_part_preserve_trailing_slash, basename_filter).
    let (dir_part, filter) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let resolved = resolve_filepaths_root(cwd, dir_part)?;
    let entries = std::fs::read_dir(&resolved).ok()?;
    let mut out: Vec<(String, String)> = Vec::new();
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
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if folders_only && !is_dir {
            continue;
        }
        let trailing = if is_dir { "/" } else { "" };
        let insertion = format!("{dir_part}{name}{trailing}");
        let display = format!("{name}{trailing}");
        out.push((insertion, display));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Some(out)
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

fn name_or_aliases_match(name: &str, aliases: &[String], prefix: &str) -> bool {
    if name.starts_with(prefix) {
        return true;
    }
    aliases.iter().any(|a| a.starts_with(prefix))
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
                        suggestions: vec!["main".into(), "dev".into(), "feature/x".into()],
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
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["checkout", "commit", "status"]);
    }

    #[test]
    fn git_co_filters_by_prefix_in_subcommand_names() {
        let r = complete("git co", 6, &registry_with(git_min()));
        // `co` matches checkout (via alias starts_with) and commit (primary).
        let names: Vec<_> = r.items.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(names, ["checkout", "commit"]);
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
}
