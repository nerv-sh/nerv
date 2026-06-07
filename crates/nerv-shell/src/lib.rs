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
    fn init_block_preserves_bin_path_with_spaces() {
        // macOS Applications path can have spaces. The eval line
        // should round-trip them verbatim.
        let b = init_block("/Applications/My Tools/nerv", "1.0.0", "ts", "zsh");
        assert!(b.contains("/Applications/My Tools/nerv"));
        assert!(b.contains("Version: 1.0.0"));
    }
}
