//! complete — the high-level completion pipeline.
//!
//! Plumbing between `shell_parser` (tokenize the input line),
//! `spec_loader` (find the right Spec for the binary name), and
//! `spec_parser` (run the matcher to get cursor context), then emit
//! ranked [`Suggestion`]s based on the cursor context.
//!
//! v0.6 / M0-6 chunk 4: eager load of all *.json under a specs
//! directory at registry construction; lazy load arrives in M1.
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

/// Eagerly-loaded spec set, keyed by binary name (the spec's root
/// `name` field — typically also the file stem on disk).
#[derive(Debug, Default)]
pub struct SpecRegistry {
    specs: HashMap<String, Spec>,
}

impl SpecRegistry {
    /// Empty registry (useful for tests that build Specs in code).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load every `*.json` under `dir` into the registry.
    ///
    /// Returns the registry even if some files fail to parse — the
    /// vector of errors lets the caller log them without aborting
    /// daemon startup. A missing directory yields an empty registry
    /// with no errors (first-run case).
    pub fn load_dir(dir: &Path) -> (Self, Vec<SpecLoadError>) {
        let mut registry = Self::empty();
        let mut errors = Vec::new();
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return (registry, errors), // Missing dir = empty registry.
        };
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
                    registry.specs.insert(key, spec);
                }
                Err(e) => errors.push(e),
            }
        }
        (registry, errors)
    }

    /// Look up a spec by binary name.
    pub fn get(&self, name: &str) -> Option<&Spec> {
        self.specs.get(name)
    }

    /// Replace or insert a spec — for tests + future hot-reload.
    pub fn insert(&mut self, spec: Spec) {
        let key = spec.name.clone();
        self.specs.insert(key, spec);
    }

    /// Number of loaded specs.
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// `true` when no specs are loaded.
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Iterate over loaded binary names (unspecified order).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.specs.keys().map(|s| s.as_str())
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

    let Some(spec) = registry.get(binary) else {
        return CompleteResult {
            items: vec![],
            reason: Some(format!("no spec for {binary}")),
        };
    };

    let result = parse_arguments(spec, &tokens, cursor);
    let current = walk_to_current(spec, &result.subcommand_path).unwrap_or(spec);

    // When the partial token starts with `-` and the current subcommand
    // has options, emit options regardless of cursor_context. The state
    // machine doesn't see the partial token (it's after the cursor) so
    // it may report Arg/Subcommand while the user is clearly asking for
    // a flag. This mirrors the TS reference's surface-form override.
    let prefix_is_option = prefix.starts_with('-') && !current.options.is_empty();

    let items = if prefix_is_option {
        emit_options(current, &prefix)
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
    out.sort_by(|a, b| a.display.cmp(&b.display));
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
        let mut r = SpecRegistry::empty();
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
        assert!(r.get("git").is_some());
        assert!(r.get("echo").is_some());
        assert!(r.get("docker").is_some());
        assert!(r.get("kubectl").is_some());
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
