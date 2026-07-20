//! nerv-shell — zsh init script generation and marker-block management.
//!
//! This crate owns the *exact* contract documented in
//! `docs/uninstall-spec.md` §3 (marker block format) and §4 step 3
//! (atomic .zshrc edits). The matching uninstall logic in `nerv-cli`
//! reuses these routines.
//!
//! Critical invariants:
//! - Markers `# >>> nerv >>>` and `# <<< nerv <<<` are **fixed strings**.
//! - `init_block(...)` output is always wrapped between exactly one pair.
//! - `strip_blocks(...)` removes *every* pair found (idempotent).

#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]

/// Marker that opens a Nerv-managed block in `~/.zshrc`.
pub const MARKER_START: &str = "# >>> nerv >>>";

/// Marker that closes a Nerv-managed block in `~/.zshrc`.
pub const MARKER_END: &str = "# <<< nerv <<<";

/// Generate the rc-file block that `nerv init <shell> >> ~/.<shell>rc`
/// produces.
///
/// `bin_path` is the absolute path to the `nerv` binary (typically
/// `/opt/homebrew/bin/nerv` from Homebrew). `version` and `installed_at`
/// are written into the metadata comment lines. `shell` is the shell name
/// (`zsh` / `bash`) used in the managed comment and the `eval` line — the
/// `# >>> nerv >>>` markers themselves stay identical across shells so the
/// uninstaller strips both the same way.
///
/// The output starts with `MARKER_START` and ends with `MARKER_END`, no
/// trailing newline beyond the closing marker line. Callers append as
/// appropriate.
pub fn init_block(
    bin_path: &str,
    version: &str,
    installed_at_iso8601: &str,
    shell: &str,
) -> String {
    // fish is not POSIX: it sources command output via `| source`, not
    // the POSIX `eval "$(...)"`. zsh/bash use the POSIX form.
    let eval_line = if shell == "fish" {
        format!("{bin_path} init fish --shell-script | source")
    } else {
        format!("eval \"$({bin_path} init {shell} --shell-script)\"")
    };
    format!(
        "{start}\n\
         # Managed by `nerv init {shell}`. Do not edit between markers.\n\
         # Version: {version}\n\
         # Installed: {when}\n\
         {eval_line}\n\
         {end}\n",
        start = MARKER_START,
        end = MARKER_END,
        shell = shell,
        version = version,
        when = installed_at_iso8601,
        eval_line = eval_line,
    )
}

/// Remove *every* Nerv marker block from `content`, returning the result.
///
/// This is intentionally tolerant: lines outside the markers are
/// preserved exactly (whitespace, comments, user code). Multiple
/// blocks are all removed (uninstall-spec.md §7.9 guarantees this).
pub fn strip_blocks(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut inside = false;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if !inside && trimmed == MARKER_START {
            inside = true;
            continue;
        }
        if inside {
            if trimmed == MARKER_END {
                inside = false;
            }
            // skip everything between (and the end marker itself)
            continue;
        }
        out.push_str(line);
    }
    // If the file ended mid-block (corruption case), do nothing extra —
    // we leave the partial trailing content out, matching uninstall
    // semantics ("trace zero").
    out
}

/// What [`upsert_block`] decided to do with the rc content.
#[derive(Debug, PartialEq, Eq)]
pub enum UpsertAction {
    /// No block existed — one was appended.
    Installed,
    /// A block existed but its version or binary path differed (or the
    /// file held duplicate blocks) — replaced with exactly one fresh
    /// block. `from` is the previous block's `# Version:` value, if
    /// parseable.
    Updated { from: Option<String> },
    /// A single block with the same version + binary path is already
    /// present — content returned unchanged.
    Current,
}

/// Result of planning an idempotent rc-file update: the full new file
/// content plus what happened. Pure — no I/O; the caller owns the
/// atomic write (uninstall-spec §4 step 3).
#[derive(Debug)]
pub struct BlockUpsert {
    pub content: String,
    pub action: UpsertAction,
}

/// The identity lines of a block: (`# Version:` value, the `eval`/
/// `source` line). Timestamp (`# Installed:`) is deliberately ignored so
/// a no-op re-run preserves the original install date.
fn block_identity(content: &str) -> (Option<String>, Option<String>) {
    let mut inside = false;
    let mut version = None;
    let mut eval_line = None;
    for line in content.lines() {
        let t = line.trim_end_matches(['\r', '\n']);
        if !inside && t == MARKER_START {
            inside = true;
            continue;
        }
        if inside {
            if t == MARKER_END {
                break;
            }
            if let Some(v) = t.strip_prefix("# Version: ") {
                version = Some(v.to_string());
            } else if !t.starts_with('#') && !t.is_empty() {
                eval_line = Some(t.to_string());
            }
        }
    }
    (version, eval_line)
}

/// Plan an idempotent insert/update of `block` into rc-file `existing`
/// (the contract behind `eval "$(nerv init <shell>)"` — first-5-min
/// §0.5-C):
///
/// - no block present → append one ([`UpsertAction::Installed`]),
/// - exactly one block with the same version + binary path →
///   leave the file byte-identical ([`UpsertAction::Current`]),
/// - anything else (older version, moved binary, duplicate blocks) →
///   strip every block and append exactly one fresh copy
///   ([`UpsertAction::Updated`]).
pub fn upsert_block(existing: &str, block: &str) -> BlockUpsert {
    let count = count_blocks(existing);
    if count == 1 && block_identity(existing) == block_identity(block) {
        return BlockUpsert {
            content: existing.to_string(),
            action: UpsertAction::Current,
        };
    }
    let (old_version, _) = block_identity(existing);
    let mut content = strip_blocks(existing);
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(block);
    let action = if count == 0 {
        UpsertAction::Installed
    } else {
        UpsertAction::Updated { from: old_version }
    };
    BlockUpsert { content, action }
}

/// Count how many marker blocks appear in `content`. Used by tests
/// and `nerv doctor` to verify idempotency invariants.
pub fn count_blocks(content: &str) -> usize {
    content
        .lines()
        .filter(|l| l.trim_end_matches(['\r', '\n']) == MARKER_START)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_block_is_well_formed() {
        let b = init_block(
            "/opt/homebrew/bin/nerv",
            "1.0.0",
            "2026-04-29T15:30:00Z",
            "zsh",
        );
        assert!(b.starts_with(MARKER_START));
        assert!(b.contains("Version: 1.0.0"));
        assert!(b.contains("eval \"$(/opt/homebrew/bin/nerv init zsh --shell-script)\""));
        assert!(b.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn init_block_bash_uses_bash_in_eval_and_markers_unchanged() {
        let b = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "bash");
        // Markers are shell-agnostic (uninstaller strips both the same).
        assert!(b.starts_with(MARKER_START));
        assert!(b.trim_end().ends_with(MARKER_END));
        // The eval + managed comment reference bash, not zsh.
        assert!(b.contains("eval \"$(/opt/homebrew/bin/nerv init bash --shell-script)\""));
        assert!(b.contains("Managed by `nerv init bash`"));
        assert!(!b.contains("init zsh"));
    }

    #[test]
    fn init_block_fish_uses_source_pipe_not_posix_eval() {
        let b = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "fish");
        assert!(b.starts_with(MARKER_START));
        assert!(b.trim_end().ends_with(MARKER_END));
        // fish sources via `| source`, never the POSIX `eval "$(...)"`.
        assert!(b.contains("/opt/homebrew/bin/nerv init fish --shell-script | source"));
        assert!(!b.contains("eval \"$("));
        assert!(b.contains("Managed by `nerv init fish`"));
    }

    #[test]
    fn strip_removes_block_and_preserves_user_code() {
        let zshrc = format!(
            "export PATH=/usr/local/bin:$PATH\n\
             alias ll=\"ls -la\"\n\
             {block}\
             # user comment after\n",
            block = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh"),
        );
        let out = strip_blocks(&zshrc);
        assert_eq!(count_blocks(&out), 0);
        assert!(out.contains("export PATH=/usr/local/bin:$PATH"));
        assert!(out.contains("alias ll=\"ls -la\""));
        assert!(out.contains("# user comment after"));
    }

    #[test]
    fn strip_removes_multiple_blocks_idempotently() {
        let blk = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let zshrc = format!("a\n{blk}b\n{blk}c\n");
        let stripped = strip_blocks(&zshrc);
        assert_eq!(count_blocks(&stripped), 0);
        assert!(stripped.contains('a'));
        assert!(stripped.contains('b'));
        assert!(stripped.contains('c'));
    }

    #[test]
    fn strip_does_not_touch_unmarked_nerv_mention() {
        let zshrc = "alias nerv-test=echo\n";
        assert_eq!(strip_blocks(zshrc), zshrc);
    }

    #[test]
    fn strip_on_empty_input_returns_empty() {
        assert_eq!(strip_blocks(""), "");
    }

    #[test]
    fn strip_handles_only_block_no_user_code() {
        let zshrc = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        assert_eq!(strip_blocks(&zshrc), "");
    }

    #[test]
    fn strip_drops_unterminated_block_to_end() {
        // Corruption case: start marker without end. Everything from
        // the start marker through EOF gets dropped — matches the
        // "trace zero" uninstall guarantee.
        let zshrc = format!(
            "user code line\n\
             {MARKER_START}\n\
             # half-written block, no end marker\n\
             eval \"$(nerv init zsh --shell-script)\"\n"
        );
        let stripped = strip_blocks(&zshrc);
        assert_eq!(stripped, "user code line\n");
        assert_eq!(count_blocks(&stripped), 0);
    }

    #[test]
    fn strip_preserves_marker_text_inside_string_literal() {
        // The matcher is line-exact (trimmed CR/LF). A marker buried
        // inside a longer line — e.g. echoed inside an alias — must
        // NOT trigger stripping.
        let zshrc = format!("alias x='echo {MARKER_START} hello'\n");
        assert_eq!(strip_blocks(&zshrc), zshrc);
    }

    #[test]
    fn count_blocks_counts_start_markers_only() {
        let blk = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let zshrc = format!("a\n{blk}b\n{blk}c\n{blk}");
        assert_eq!(count_blocks(&zshrc), 3);
        assert_eq!(count_blocks(""), 0);
    }

    #[test]
    fn strip_tolerates_crlf_line_endings() {
        // Windows-style \r\n on every line. trim_end_matches drops
        // both CR and LF, so marker detection still works.
        let blk_lf = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let zshrc_crlf = format!("a\r\n{}", blk_lf.replace('\n', "\r\n"));
        let out = strip_blocks(&zshrc_crlf);
        assert_eq!(count_blocks(&out), 0);
        assert!(out.starts_with("a\r\n"));
    }

    #[test]
    fn upsert_installs_into_fresh_rc() {
        let blk = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let up = upsert_block("export PATH=$PATH\n", &blk);
        assert_eq!(up.action, UpsertAction::Installed);
        assert_eq!(count_blocks(&up.content), 1);
        assert!(up.content.starts_with("export PATH=$PATH\n"));
        assert!(up.content.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn upsert_same_version_and_bin_is_current_and_byte_identical() {
        // Timestamps differ between the two init_block calls — identity
        // ignores them, so a re-run is a no-op (preserves install date).
        let blk_v1 = init_block(
            "/opt/homebrew/bin/nerv",
            "1.0.0",
            "2026-01-01T00:00:00Z",
            "zsh",
        );
        let rc = format!("user stuff\n{blk_v1}");
        let blk_rerun = init_block(
            "/opt/homebrew/bin/nerv",
            "1.0.0",
            "2026-07-20T12:00:00Z",
            "zsh",
        );
        let up = upsert_block(&rc, &blk_rerun);
        assert_eq!(up.action, UpsertAction::Current);
        assert_eq!(up.content, rc, "no-op must not rewrite the file");
    }

    #[test]
    fn upsert_new_version_updates_and_reports_old() {
        let old = init_block("/opt/homebrew/bin/nerv", "0.9.0", "ts", "zsh");
        let rc = format!("a\n{old}b\n");
        let new = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts2", "zsh");
        let up = upsert_block(&rc, &new);
        assert_eq!(
            up.action,
            UpsertAction::Updated {
                from: Some("0.9.0".into())
            }
        );
        assert_eq!(count_blocks(&up.content), 1);
        assert!(up.content.contains("Version: 1.0.0"));
        assert!(!up.content.contains("Version: 0.9.0"));
        // User lines around the old block survive.
        assert!(up.content.contains("a\n") && up.content.contains("b\n"));
    }

    #[test]
    fn upsert_moved_binary_updates_even_on_same_version() {
        // brew upgrade keeps the version dir moving; a stale bin path in
        // the eval line means dead completions — must refresh.
        let old = init_block("/old/path/nerv", "1.0.0", "ts", "zsh");
        let new = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let up = upsert_block(&old, &new);
        assert!(matches!(up.action, UpsertAction::Updated { .. }));
        assert!(up.content.contains("/opt/homebrew/bin/nerv"));
        assert!(!up.content.contains("/old/path/nerv"));
    }

    #[test]
    fn upsert_dedupes_multiple_blocks_to_one() {
        let blk = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let rc = format!("{blk}mid\n{blk}");
        let up = upsert_block(&rc, &blk);
        assert!(matches!(up.action, UpsertAction::Updated { .. }));
        assert_eq!(count_blocks(&up.content), 1);
        assert!(up.content.contains("mid\n"));
    }

    #[test]
    fn upsert_appends_newline_separator_when_rc_lacks_one() {
        let blk = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts", "zsh");
        let up = upsert_block("no trailing newline", &blk);
        assert!(up.content.starts_with("no trailing newline\n"));
        assert_eq!(count_blocks(&up.content), 1);
    }

    #[test]
    fn init_block_preserves_bin_path_with_spaces() {
        // macOS Applications path can have spaces. The eval line
        // should round-trip them verbatim.
        let b = init_block("/Applications/My Tools/nerv", "1.0.0", "ts", "zsh");
        assert!(b.contains("/Applications/My Tools/nerv"));
        assert!(b.contains("Version: 1.0.0"));
    }
}
