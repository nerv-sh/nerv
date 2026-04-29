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

/// Generate the `~/.zshrc` block that `nerv init zsh >> ~/.zshrc` produces.
///
/// `bin_path` is the absolute path to the `nerv` binary (typically
/// `/opt/homebrew/bin/nerv` from Homebrew). `version` and `installed_at`
/// are written into the metadata comment lines.
///
/// The output starts with `MARKER_START` and ends with `MARKER_END`, no
/// trailing newline beyond the closing marker line. Callers append as
/// appropriate.
pub fn init_block(bin_path: &str, version: &str, installed_at_iso8601: &str) -> String {
    format!(
        "{start}\n\
         # Managed by `nerv init zsh`. Do not edit between markers.\n\
         # Version: {version}\n\
         # Installed: {when}\n\
         eval \"$({bin} init zsh --shell-script)\"\n\
         {end}\n",
        start = MARKER_START,
        end = MARKER_END,
        version = version,
        when = installed_at_iso8601,
        bin = bin_path,
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
        let b = init_block("/opt/homebrew/bin/nerv", "1.0.0", "2026-04-29T15:30:00Z");
        assert!(b.starts_with(MARKER_START));
        assert!(b.contains("Version: 1.0.0"));
        assert!(b.contains("eval \"$(/opt/homebrew/bin/nerv init zsh --shell-script)\""));
        assert!(b.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn strip_removes_block_and_preserves_user_code() {
        let zshrc = format!(
            "export PATH=/usr/local/bin:$PATH\n\
             alias ll=\"ls -la\"\n\
             {block}\
             # user comment after\n",
            block = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts"),
        );
        let out = strip_blocks(&zshrc);
        assert_eq!(count_blocks(&out), 0);
        assert!(out.contains("export PATH=/usr/local/bin:$PATH"));
        assert!(out.contains("alias ll=\"ls -la\""));
        assert!(out.contains("# user comment after"));
    }

    #[test]
    fn strip_removes_multiple_blocks_idempotently() {
        let blk = init_block("/opt/homebrew/bin/nerv", "1.0.0", "ts");
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
}
