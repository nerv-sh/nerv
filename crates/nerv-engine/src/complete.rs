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
        if let Some(opt) = current.options.iter().find(|o| o.names.contains(opt_name)) {
            if let Some(arg) = opt.args.get(*arg_idx) {
                let items = emit_candidates_for_arg(arg, &prefix, cwd, Some(opt));
                return CompleteResult {
                    items,
                    reason: None,
                };
            }
        }
    }

    let items = if prefix_is_option {
        emit_options_with_ancestors(current, &ancestor_refs, &prefix)
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
            CursorContext::OptionName => {
                emit_options_with_ancestors(current, &ancestor_refs, &prefix)
            }
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
            priority: sc.priority,
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
) -> Vec<Suggestion> {
    let emit = |opt: &crate::spec_parser::Opt| -> Vec<Suggestion> {
        opt.names
            .iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| Suggestion {
                insertion: n.clone(),
                display: n.clone(),
                description: opt.description.clone(),
                kind: SuggestionKind::Flag,
                priority: opt.priority,
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
            if opt.names.iter().any(|n| {
                node.options
                    .iter()
                    .any(|local| local.names.contains(n))
            }) {
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
) -> Vec<Suggestion> {
    let Some(arg) = node.args.first() else {
        return vec![];
    };
    emit_candidates_for_arg(arg, prefix, cwd, None)
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
) -> Vec<Suggestion> {
    let enclosing_description = enclosing_opt.and_then(|o| o.description.as_deref());
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
        .filter(|s| s.starts_with(prefix))
        .map(|s| Suggestion {
            insertion: s.clone(),
            display: s,
            description: desc_for_extract.map(|d| d.to_string()),
            kind: SuggestionKind::Argument,
            priority: None,
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
            .filter(|s| s.name.starts_with(prefix))
            .map(|s| Suggestion {
                insertion: s.insert_value.clone().unwrap_or_else(|| s.name.clone()),
                display: s.display_name.clone().unwrap_or_else(|| s.name.clone()),
                description: s.description.clone(),
                kind: SuggestionKind::Argument,
                priority: s.priority,
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
            out.extend(paths.into_iter().map(|(insertion, display, description)| Suggestion {
                insertion,
                display,
                description,
                kind: SuggestionKind::Argument,
                priority: None,
            }));
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
                    .filter(|s| s.starts_with(prefix))
                    .map(|s| Suggestion {
                        insertion: s.clone(),
                        display: s,
                        description: Some("history".into()),
                        kind: SuggestionKind::Argument,
                        priority: None,
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
                    if let Some(lines) = cached_template_generator(script) {
                        out.extend(
                            lines
                                .into_iter()
                                .map(|line| split_id_label(&line))
                                .filter(|(ins, _)| ins.starts_with(prefix))
                                .map(|(insertion, display)| Suggestion {
                                    insertion,
                                    display,
                                    description: None,
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                }),
                        );
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
                                    priority: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::Filepaths { folders_only } => {
                    if let Some(paths) = filepaths_at(cwd, prefix, *folders_only) {
                        out.extend(paths.into_iter().map(|(insertion, display, description)| Suggestion {
                            insertion,
                            display,
                            description,
                            kind: SuggestionKind::Argument,
                            priority: None,
                        }));
                    }
                }
                crate::spec_parser::Generator::SshHosts => {
                    if let Some(hosts) = ssh_hosts() {
                        out.extend(
                            hosts
                                .into_iter()
                                .filter(|h| h.starts_with(prefix))
                                .map(|h| Suggestion {
                                    insertion: h.clone(),
                                    display: h,
                                    description: Some("SSH host".into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::MakefileTargets => {
                    if let Some(targets) = makefile_targets(cwd) {
                        out.extend(targets.into_iter().filter(|t| t.starts_with(prefix)).map(
                            |t| Suggestion {
                                insertion: t.clone(),
                                display: t,
                                description: Some("make target".into()),
                                kind: SuggestionKind::Argument,
                                priority: None,
                            },
                        ));
                    }
                }
                crate::spec_parser::Generator::ManPages => {
                    if let Some(pages) = man_pages() {
                        out.extend(
                            pages
                                .into_iter()
                                .filter(|p| p.starts_with(prefix))
                                .map(|p| Suggestion {
                                    insertion: p.clone(),
                                    display: p,
                                    description: Some("man page".into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::PackageJsonDeps => {
                    if let Some(deps) = package_json_deps(cwd) {
                        out.extend(
                            deps.into_iter()
                                .filter(|(name, _)| name.starts_with(prefix))
                                .map(|(name, kind)| Suggestion {
                                    insertion: name.clone(),
                                    display: name,
                                    description: Some(kind.into()),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
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
                    if let Some(lines) = cached_template_generator(&key) {
                        out.extend(lines.into_iter().filter(|s| s.starts_with(prefix)).map(
                            |line| Suggestion {
                                insertion: line.clone(),
                                display: line,
                                description: Some("k8s resource".into()),
                                kind: SuggestionKind::Argument,
                                priority: None,
                            },
                        ));
                    }
                }
                crate::spec_parser::Generator::CargoTargets { kind } => {
                    if let Some(targets) = cargo_targets(cwd, kind.as_deref()) {
                        out.extend(
                            targets
                                .into_iter()
                                .filter(|(name, _, _)| name.starts_with(prefix))
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
                                }),
                        );
                    }
                }
                crate::spec_parser::Generator::ZoxideQuery => {
                    if let Some(rows) = zoxide_query() {
                        // z / zoxide are fuzzy by design — `z claud`
                        // should match `~/.claude` even though the
                        // folder name is `.claude`. Filter by
                        // substring (case-insensitive) on either the
                        // folder name OR the full path.
                        let needle = prefix.to_lowercase();
                        out.extend(
                            rows.into_iter()
                                .filter(|(name, path, _)| {
                                    needle.is_empty()
                                        || name.to_lowercase().contains(&needle)
                                        || path.to_lowercase().contains(&needle)
                                })
                                .map(|(name, path, score)| Suggestion {
                                    insertion: name.clone(),
                                    display: name,
                                    description: Some(format!("{path} (score {score:.1})")),
                                    kind: SuggestionKind::Argument,
                                    priority: None,
                                }),
                        );
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
                        .map(|(insertion, display, description)| Suggestion {
                            insertion,
                            display,
                            description,
                            kind: SuggestionKind::Argument,
                            priority: None,
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
        "path" | "file" | "files" | "filepath" | "filename" | "src" | "dest"
        | "source" | "destination" | "input" | "output" => Some(false),
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
fn infer_filepaths_kind_from_opt_names(
    opt: Option<&crate::spec_parser::Opt>,
) -> Option<bool> {
    let opt = opt?;
    let names: Vec<String> = opt.names.iter().map(|n| n.to_ascii_lowercase()).collect();
    for n in &names {
        if let Some(long) = n.strip_prefix("--") {
            match long {
                "file" | "files" | "filename" | "filepath" | "input" | "output"
                | "log" | "log-file" | "input-file" | "output-file" => return Some(false),
                "dir" | "directory" | "folder" | "input-dir" | "output-dir"
                | "workdir" | "working-dir" | "chdir" => return Some(true),
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

fn execute_template_generator(script: &[String]) -> Option<Vec<String>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
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
    // Drain stdout in a dedicated thread so the pipe buffer (~64 KB
    // on macOS) never fills and blocks the child. A previous version
    // try_wait()'d in 10ms ticks but never read the pipe — anything
    // that wrote more than ~64 KB before exiting (e.g. `ps axo
    // pid,comm` on a busy machine: 1600+ lines / ~50 KB) deadlocked
    // and tripped the 200 ms cap even when the command itself
    // finished in <100 ms.
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(8192);
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = match rx.recv_timeout(Duration::from_millis(GENERATOR_TIMEOUT_MS)) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    // Don't gate on exit status alone — many Fig generators run
    // `find $i ...` over `$PATH`-derived dirs that may not all exist,
    // so the script exits non-zero on the missing-path case even
    // though stdout was usefully populated. If we got stdout bytes,
    // use them; only return None when there's truly nothing to parse.
    let _ = child.wait();
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

/// Split a multi-column Tier B output line into `(insertion, display)`.
/// When the first whitespace-separated token looks like a numeric id
/// (e.g. `1234 /bin/zsh` from `ps axo pid,comm`) the bare id becomes
/// the insertion and the original line stays as the display label.
/// Otherwise the full line is used as both — covers single-column
/// generators (`brew list -1`, `kubectl -o name`, etc.) where the
/// label IS the insertion.
fn split_id_label(raw: &str) -> (String, String) {
    let trimmed = raw.trim_start();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim_start();
    let id_like =
        !first.is_empty() && first.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty();
    if id_like {
        (first.to_string(), trimmed.to_string())
    } else {
        (raw.to_string(), raw.to_string())
    }
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
/// [`GENERATOR_CACHE`] because that cache routes JSON-shaped output
/// through `extract_json_candidates`, which would lose the nested
/// `packages[*].targets[*]` structure cargo_targets needs.
static CARGO_METADATA_CACHE: std::sync::LazyLock<
    std::sync::Mutex<HashMap<std::path::PathBuf, (std::time::Instant, String)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn cached_cargo_metadata(cwd: &std::path::Path) -> Option<String> {
    let canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if let Ok(cache) = CARGO_METADATA_CACHE.lock() {
        if let Some((stamp, blob)) = cache.get(&canon) {
            if stamp.elapsed() < GENERATOR_CACHE_TTL {
                return Some(blob.clone());
            }
        }
    }
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;
    let mut child = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(&canon)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(65_536);
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = match rx.recv_timeout(Duration::from_millis(GENERATOR_TIMEOUT_MS)) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let _ = child.wait();
    if buf.is_empty() {
        return None;
    }
    let blob = String::from_utf8_lossy(&buf).into_owned();
    if let Ok(mut cache) = CARGO_METADATA_CACHE.lock() {
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
    let lines = cached_template_generator(&key)?;
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
/// Returns `(insertion, display)` pairs — insertion preserves the
/// user's typed directory prefix so the widget's word-level replace
/// doesn't lose context (`cd ./fo<Tab>` → `cd ./encl/`, not `cd encl/`).
fn filepaths_at(
    cwd: Option<&std::path::Path>,
    prefix: &str,
    folders_only: bool,
) -> Option<Vec<(String, String, Option<String>)>> {
    // Split prefix into (dir_part_preserve_trailing_slash, basename_filter).
    let (dir_part, filter) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let resolved = resolve_filepaths_root(cwd, dir_part)?;
    let entries = std::fs::read_dir(&resolved).ok()?;
    let mut out: Vec<(String, String, Option<String>)> = Vec::new();
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
        // Description fallback: file size for regular files, "dir"
        // for folders, "→ target" for symlinks. One metadata() per
        // entry — cheap (~10µs each, ~500µs for a 50-entry dir).
        // Keeps the footer line in the popup informative; was
        // empty before for cd / ls / cat / vim / ...
        let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
        let desc = filepaths_desc(&e, is_dir, is_symlink);
        out.push((insertion, display, desc));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Some(out)
}

fn filepaths_desc(
    entry: &std::fs::DirEntry,
    is_dir: bool,
    is_symlink: bool,
) -> Option<String> {
    if is_symlink {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            return Some(format!("→ {}", target.display()));
        }
        return Some("symlink".into());
    }
    if is_dir {
        return Some("dir".into());
    }
    let meta = entry.metadata().ok()?;
    Some(human_size(meta.len()))
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
        assert_eq!(
            extract_enum_from_description("Path: {file}"),
            None
        );
    }

    #[test]
    fn extract_enum_ignores_braces_with_disallowed_chars() {
        // `{a b|c d}` has spaces inside entries — not an enum list.
        assert_eq!(
            extract_enum_from_description("usage: {a b|c d}"),
            None
        );
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
        }
    }

    #[test]
    fn priority_sort_higher_first() {
        let mut v = vec![
            sug("a", Some(50)),
            sug("b", Some(75)),
            sug("c", Some(25)),
        ];
        v.sort_by(sort_by_priority_then_alpha);
        assert_eq!(v[0].display, "b");
        assert_eq!(v[1].display, "a");
        assert_eq!(v[2].display, "c");
    }

    #[test]
    fn priority_sort_default_50_ties_break_alpha() {
        let mut v = vec![sug("zeta", None), sug("alpha", None)];
        v.sort_by(sort_by_priority_then_alpha);
        assert_eq!(v[0].display, "alpha");
        assert_eq!(v[1].display, "zeta");
    }

    #[test]
    fn priority_sort_explicit_50_equals_none() {
        let mut v = vec![sug("a", None), sug("b", Some(50))];
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
}
