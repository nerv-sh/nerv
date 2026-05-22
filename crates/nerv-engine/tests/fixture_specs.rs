//! End-to-end fixture replay — load hand-authored JSON specs, run
//! parse_arguments against realistic command lines, assert
//! annotations + cursor context.
//!
//! These cover the first-5-min.md scenarios (git status / log /
//! checkout, echo variadic) on real Tier-A specs so any future
//! drift between spec_loader and spec_parser is caught.
//!
//! Refs: PLAN.md §10 M0-6, docs/first-5-min.md, docs/spec-conversion-policy.md

use nerv_engine::{Annotation, CursorContext, Spec, TokenKind, load_spec_file, parse_arguments};
use std::ops::Range;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> Spec {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "specs"]
        .iter()
        .collect::<PathBuf>()
        .join(format!("{name}.json"));
    load_spec_file(&path).unwrap_or_else(|e| panic!("loading {}: {e}", path.display()))
}

fn ann(text: &str, span: Range<usize>) -> Annotation {
    Annotation {
        span,
        text: text.to_string(),
        kind: TokenKind::Unknown,
    }
}

fn tokenize(line: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for word in line.split(' ') {
        if !word.is_empty() {
            let end = start + word.len();
            out.push(ann(word, start..end));
        }
        start += word.len() + 1;
    }
    out
}

#[test]
fn git_status_parses_as_subcommand_chain() {
    let g = fixture("git");
    let toks = tokenize("git status");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["git", "status"]);
    assert_eq!(r.annotations[0].kind, TokenKind::Subcommand);
    assert_eq!(r.annotations[1].kind, TokenKind::Subcommand);
    // status has options but no subcommands → OptionName.
    assert_eq!(r.cursor_context, CursorContext::OptionName);
}

#[test]
fn git_status_short_flag_binds() {
    let g = fixture("git");
    let toks = tokenize("git status --short");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
}

#[test]
fn git_log_oneline_then_number() {
    let g = fixture("git");
    let toks = tokenize("git log --oneline -n 5");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.annotations[0].kind, TokenKind::Subcommand);
    assert_eq!(r.annotations[1].kind, TokenKind::Subcommand);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // --oneline
    assert_eq!(r.annotations[3].kind, TokenKind::OptionName); // -n
    assert_eq!(r.annotations[4].kind, TokenKind::OptionArg); // 5
}

#[test]
fn git_checkout_via_alias_co() {
    let g = fixture("git");
    let toks = tokenize("git co main");
    let r = parse_arguments(&g, &toks, 999);
    // `co` is an alias for checkout; resolves to checkout's primary name.
    assert_eq!(r.subcommand_path, vec!["git", "checkout"]);
    assert_eq!(r.annotations[1].kind, TokenKind::Subcommand);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
}

#[test]
fn git_commit_message_equals_form() {
    let g = fixture("git");
    let toks = tokenize("git commit --message=fix");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
}

#[test]
fn git_commit_verbose_is_repeatable() {
    let g = fixture("git");
    let toks = tokenize("git commit -v -v -v");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[3].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[4].kind, TokenKind::OptionName);
}

#[test]
fn echo_variadic_keeps_accepting() {
    let e = fixture("echo");
    let toks = tokenize("echo hello world again");
    let r = parse_arguments(&e, &toks, 999);
    assert_eq!(r.annotations[1].kind, TokenKind::SubcommandArg);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
    assert_eq!(r.cursor_context, CursorContext::Arg);
}

#[test]
fn echo_n_flag_before_args() {
    let e = fixture("echo");
    let toks = tokenize("echo -n hello");
    let r = parse_arguments(&e, &toks, 999);
    assert_eq!(r.annotations[1].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
}

#[test]
fn cursor_inside_subcommand_token_returns_subcommand_context() {
    let g = fixture("git");
    // "git st" with cursor at the 'st' partial.
    let toks = tokenize("git st");
    let r = parse_arguments(&g, &toks, 4);
    assert_eq!(r.subcommand_path, vec!["git"]);
    assert_eq!(r.cursor_context, CursorContext::Subcommand);
}

#[test]
fn cursor_after_option_name_expects_arg_for_options_with_args() {
    let g = fixture("git");
    // "git commit -m " with cursor past the -m flag.
    let toks = tokenize("git commit -m");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    // After -m, an arg is expected.
    assert_eq!(r.cursor_context, CursorContext::Arg);
}

#[test]
fn fixture_path_resolution_via_env_macro() {
    // Sanity: make sure CARGO_MANIFEST_DIR resolves to the crate root.
    let p: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("specs")
        .join("git.json");
    assert!(p.exists(), "git fixture missing: {}", p.display());
}
