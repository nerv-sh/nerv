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

// ---- M0 Go/No-Go: docker scenarios -----------------------------------

#[test]
fn docker_ps_subcommand_chain() {
    let d = fixture("docker");
    let toks = tokenize("docker ps");
    let r = parse_arguments(&d, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["docker", "ps"]);
    assert_eq!(r.cursor_context, CursorContext::OptionName);
}

#[test]
fn docker_ps_flag_binding() {
    let d = fixture("docker");
    let toks = tokenize("docker ps --all");
    let r = parse_arguments(&d, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
}

#[test]
fn docker_run_with_name_and_image() {
    let d = fixture("docker");
    let toks = tokenize("docker run --name myapp nginx");
    let r = parse_arguments(&d, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // --name
    assert_eq!(r.annotations[3].kind, TokenKind::OptionArg); // myapp
    assert_eq!(r.annotations[4].kind, TokenKind::SubcommandArg); // nginx
}

#[test]
fn docker_run_repeatable_publish() {
    let d = fixture("docker");
    let toks = tokenize("docker run -p 80:80 -p 443:443 nginx");
    let r = parse_arguments(&d, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // -p
    assert_eq!(r.annotations[4].kind, TokenKind::OptionName); // -p (repeatable)
    assert_eq!(r.annotations[6].kind, TokenKind::SubcommandArg); // nginx
}

#[test]
fn docker_build_with_tag_and_path() {
    let d = fixture("docker");
    let toks = tokenize("docker build -t myimage:latest .");
    let r = parse_arguments(&d, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // -t
    assert_eq!(r.annotations[3].kind, TokenKind::OptionArg); // myimage:latest
    assert_eq!(r.annotations[4].kind, TokenKind::SubcommandArg); // .
}

#[test]
fn docker_compose_nested_subcommands() {
    let d = fixture("docker");
    let toks = tokenize("docker compose up");
    let r = parse_arguments(&d, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["docker", "compose", "up"]);
}

// ---- M0 Go/No-Go: kubectl scenarios ----------------------------------

#[test]
fn kubectl_get_pods() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl get pods");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["kubectl", "get"]);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
}

#[test]
fn kubectl_get_namespace_short_alias() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl get po -n default");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg); // po
    assert_eq!(r.annotations[3].kind, TokenKind::OptionName); // -n
    assert_eq!(r.annotations[4].kind, TokenKind::OptionArg); // default
}

#[test]
fn kubectl_describe_with_namespace() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl describe pods my-pod -n prod");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["kubectl", "describe"]);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg); // pods
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg); // my-pod (variadic)
    assert_eq!(r.annotations[4].kind, TokenKind::OptionName); // -n
}

#[test]
fn kubectl_logs_follow() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl logs -f my-pod");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // -f
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg); // my-pod
}

#[test]
fn kubectl_apply_with_filename() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl apply -f deployment.yaml");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName); // -f
    assert_eq!(r.annotations[3].kind, TokenKind::OptionArg); // deployment.yaml
}

#[test]
fn kubectl_rollout_status_nested() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl rollout status");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["kubectl", "rollout", "status"]);
}

#[test]
fn kubectl_config_use_context() {
    let k = fixture("kubectl");
    let toks = tokenize("kubectl config use-context");
    let r = parse_arguments(&k, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["kubectl", "config", "use-context"]);
}

// ---- Extended fixtures: npm / cargo / gh / brew / make --------------

#[test]
fn npm_install_with_alias() {
    let n = fixture("npm");
    let toks = tokenize("npm i react");
    let r = parse_arguments(&n, &toks, 999);
    // `i` is alias for install.
    assert_eq!(r.subcommand_path, vec!["npm", "install"]);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
}

#[test]
fn npm_install_save_dev_flag() {
    let n = fixture("npm");
    let toks = tokenize("npm install -D typescript");
    let r = parse_arguments(&n, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
}

#[test]
fn npm_run_script_arg() {
    let n = fixture("npm");
    let toks = tokenize("npm run test");
    let r = parse_arguments(&n, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
}

#[test]
fn npm_version_enum_suggestions_listed() {
    let n = fixture("npm");
    // `npm version ` cursor on the arg: parse_arguments resolves to Arg
    let toks = tokenize("npm version");
    let r = parse_arguments(&n, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["npm", "version"]);
    assert_eq!(r.cursor_context, CursorContext::Arg);
}

#[test]
fn cargo_build_release_flag() {
    let c = fixture("cargo");
    let toks = tokenize("cargo build --release");
    let r = parse_arguments(&c, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["cargo", "build"]);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
}

#[test]
fn cargo_test_with_workspace() {
    let c = fixture("cargo");
    let toks = tokenize("cargo test --workspace");
    let r = parse_arguments(&c, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
}

#[test]
fn cargo_run_alias_r() {
    let c = fixture("cargo");
    let toks = tokenize("cargo r --bin nervd");
    let r = parse_arguments(&c, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["cargo", "run"]);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[3].kind, TokenKind::OptionArg);
}

#[test]
fn cargo_new_with_lib_flag() {
    let c = fixture("cargo");
    let toks = tokenize("cargo new --lib mycrate");
    let r = parse_arguments(&c, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
}

#[test]
fn gh_pr_create_nested() {
    let g = fixture("gh");
    let toks = tokenize("gh pr create");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["gh", "pr", "create"]);
}

#[test]
fn gh_pr_create_with_title() {
    let g = fixture("gh");
    let toks = tokenize("gh pr create --title foo");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.annotations[3].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[4].kind, TokenKind::OptionArg);
}

#[test]
fn gh_issue_list_alias_ls() {
    let g = fixture("gh");
    let toks = tokenize("gh issue ls");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["gh", "issue", "list"]);
}

#[test]
fn gh_auth_status_three_deep() {
    let g = fixture("gh");
    let toks = tokenize("gh auth status");
    let r = parse_arguments(&g, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["gh", "auth", "status"]);
    assert_eq!(r.cursor_context, CursorContext::Done);
}

#[test]
fn brew_install_cask() {
    let b = fixture("brew");
    let toks = tokenize("brew install --cask iterm2");
    let r = parse_arguments(&b, &toks, 999);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
}

#[test]
fn brew_services_start_nested() {
    let b = fixture("brew");
    let toks = tokenize("brew services start postgresql");
    let r = parse_arguments(&b, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["brew", "services", "start"]);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
}

#[test]
fn brew_uninstall_alias_rm() {
    let b = fixture("brew");
    let toks = tokenize("brew rm node");
    let r = parse_arguments(&b, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["brew", "uninstall"]);
}

#[test]
fn make_variadic_targets() {
    let m = fixture("make");
    let toks = tokenize("make clean build test");
    let r = parse_arguments(&m, &toks, 999);
    assert_eq!(r.subcommand_path, vec!["make"]);
    assert_eq!(r.annotations[1].kind, TokenKind::SubcommandArg);
    assert_eq!(r.annotations[2].kind, TokenKind::SubcommandArg);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
    assert_eq!(r.cursor_context, CursorContext::Arg);
}

#[test]
fn make_root_level_flag_with_arg() {
    let m = fixture("make");
    let toks = tokenize("make -j 4");
    let r = parse_arguments(&m, &toks, 999);
    assert_eq!(r.annotations[1].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionArg);
}

#[test]
fn make_directory_flag() {
    let m = fixture("make");
    let toks = tokenize("make -C build install");
    let r = parse_arguments(&m, &toks, 999);
    assert_eq!(r.annotations[1].kind, TokenKind::OptionName);
    assert_eq!(r.annotations[2].kind, TokenKind::OptionArg);
    assert_eq!(r.annotations[3].kind, TokenKind::SubcommandArg);
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
