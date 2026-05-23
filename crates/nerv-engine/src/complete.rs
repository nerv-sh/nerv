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
#[derive(Debug, Default)]
pub struct SpecRegistry {
    dir: Option<PathBuf>,
    cache: RwLock<HashMap<String, Option<Arc<Spec>>>>,
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
            match load_spec_file(&path) {
                Ok(spec) => {
                    let key = if !spec.name.is_empty() {
                        spec.name.clone()
                    } else if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        stem.to_string()
                    } else {
                        continue;
                    };
                    cache.insert(key, Some(Arc::new(spec)));
                }
                Err(e) => errors.push(e),
            }
        }
        drop(cache);
        (registry, errors)
    }

    /// Resolve a spec by binary name, loading from disk on first hit.
    /// Returns `None` if the spec doesn't exist or failed to parse.
    pub fn lookup(&self, name: &str) -> Option<Arc<Spec>> {
        if let Some(cached) = self.cache.read().ok().and_then(|c| c.get(name).cloned()) {
            return cached;
        }
        let loaded = self.load_from_disk(name);
        // Negative-cache misses too, so we don't reread on each request
        // for a binary that has no spec.
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(name.to_string(), loaded.clone());
        }
        loaded
    }

    fn load_from_disk(&self, name: &str) -> Option<Arc<Spec>> {
        let dir = self.dir.as_ref()?;
        // Prefer plain JSON for human inspection; fall back to gzipped
        // form (build-time compressed cache).
        let plain = dir.join(format!("{name}.json"));
        if plain.exists() {
            return load_spec_file(&plain).ok().map(Arc::new);
        }
        let gz = dir.join(format!("{name}.json.gz"));
        if gz.exists() {
            return load_spec_file(&gz).ok().map(Arc::new);
        }
        None
    }

    /// Insert a spec into the cache directly. Used by tests that
    /// build Specs in code and by future hot-reload paths.
    pub fn insert(&self, spec: Spec) {
        let key = spec.name.clone();
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(key, Some(Arc::new(spec)));
        }
    }

    /// Count of positive cache entries. Does not include negative hits
    /// or specs that have not been looked up yet (lazy).
    pub fn len(&self) -> usize {
        self.cache
            .read()
            .map(|c| c.values().filter(|v| v.is_some()).count())
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
                    .filter(|(_, v)| v.is_some())
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
    let cursor = cursor.min(line.len());
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
        emit_subcommands(current, &prefix)
    } else {
        match result.cursor_context {
            CursorContext::Subcommand => emit_subcommands(current, &prefix),
            CursorContext::OptionName => emit_options(current, &prefix),
            CursorContext::Arg => emit_arg_candidates(current, &prefix),
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

fn emit_arg_candidates(node: &Subcommand, prefix: &str) -> Vec<Suggestion> {
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
            if let crate::spec_parser::Generator::Template { script } = g {
                if let Some(lines) = execute_template_generator(script) {
                    out.extend(
                        lines
                            .into_iter()
                            .filter(|s| s.starts_with(prefix))
                            .map(|line| Suggestion {
                                insertion: line.clone(),
                                display: line,
                                description: None,
                                kind: SuggestionKind::Argument,
                            }),
                    );
                }
            }
        }
    }

    out.sort_by(|a, b| a.display.cmp(&b.display));
    out.dedup_by(|a, b| a.display == b.display);
    out
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
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(lines)
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

    #[test]
    fn template_generator_disabled_by_env() {
        use crate::spec_parser::{Arg, Generator, Subcommand};
        // SAFETY: single-threaded test, no observers.
        unsafe { std::env::set_var("NERV_NO_GENERATORS", "1") };
        let spec = Subcommand {
            name: "x".into(),
            args: vec![Arg {
                name: Some("opt".into()),
                generators: vec![Generator::Template {
                    script: vec!["/bin/echo".into(), "should-not-run".into()],
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = complete("x ", 2, &registry_with(spec));
        unsafe { std::env::remove_var("NERV_NO_GENERATORS") };
        assert!(r.items.is_empty(), "expected no items with env disabled");
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
