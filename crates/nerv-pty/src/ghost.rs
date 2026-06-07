//! Inline "ghost text" for PTY mode (Phase 3a).
//!
//! Mirrors the M0 ZLE widget's inline preview (CLAUDE.md §3 "inline ghost
//! text"): the trailing remainder of the top suggestion is drawn after the
//! cursor in dim grey, and accepted with Right-arrow. This module is
//! pure string logic plus two escape-sequence builders — no I/O — so the
//! render decisions are unit-testable and the main loop only does the
//! actual write.
//!
//! ANSI budget (terminal-compat §3 / CLAUDE.md §4 invariant): we use only
//! DECSC/DECRC cursor save-restore (`ESC 7` / `ESC 8`), the faint SGR
//! (`ESC [ 2 m`), a reset (`ESC [ 0 m`), and erase-to-end-of-line
//! (`ESC [ K`). No alternate screen, no true colour, no OSC.

/// `ESC 7` — save cursor position (DECSC).
const SAVE: &str = "\x1b7";
/// `ESC 8` — restore cursor position (DECRC).
const RESTORE: &str = "\x1b8";
/// `ESC [ 2 m` — faint, then the text, then `ESC [ 0 m` reset.
const FAINT: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
/// `ESC [ K` — erase from cursor to end of line.
const ERASE_EOL: &str = "\x1b[K";

/// Compute the ghost remainder to display, or `None` when nothing should
/// be drawn.
///
/// `buffer` is the prompt line up to the cursor. `insertion` is the top
/// suggestion's insert string. The ghost is the part of `insertion` that
/// extends the word currently being typed:
///
/// - buffer ending in whitespace → the token is "done", show nothing
///   (matches the widget's "LBUFFER 끝이 공백이면 ghost off" rule).
/// - empty current token → nothing (don't preview before the user types).
/// - `insertion` must start with the current token (default prefix match);
///   the ghost is everything after that shared prefix.
/// - if the remainder is empty (already fully typed) → nothing.
pub fn compute_ghost(buffer: &str, insertion: &str) -> Option<String> {
    if buffer.ends_with(char::is_whitespace) {
        return None;
    }
    let token = current_token(buffer);
    if token.is_empty() {
        return None;
    }
    let remainder = insertion.strip_prefix(token)?;
    if remainder.is_empty() {
        return None;
    }
    Some(remainder.to_string())
}

/// The word currently under the cursor: everything after the last
/// whitespace run in `buffer`.
fn current_token(buffer: &str) -> &str {
    match buffer.rfind(char::is_whitespace) {
        Some(idx) => &buffer[idx + 1..],
        None => buffer,
    }
}

/// Escape sequence that draws `ghost` in faint grey immediately after the
/// cursor, leaving the cursor visually where it started. Erases to
/// end-of-line first so a previously drawn (longer) ghost can't leave a
/// stale tail behind.
pub fn render_seq(ghost: &str) -> String {
    format!("{SAVE}{ERASE_EOL}{FAINT}{ghost}{RESET}{RESTORE}")
}

/// Escape sequence that erases a previously drawn ghost: save, erase to
/// end of line, restore. Safe to emit even when no ghost is present (at a
/// prompt the region after the cursor is normally empty).
pub fn clear_seq() -> String {
    format!("{SAVE}{ERASE_EOL}{RESTORE}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghost_extends_current_token() {
        assert_eq!(
            compute_ghost("git che", "checkout").as_deref(),
            Some("ckout")
        );
    }

    #[test]
    fn ghost_uses_last_token_only() {
        assert_eq!(
            compute_ghost("git ch", "checkout").as_deref(),
            Some("eckout")
        );
    }

    #[test]
    fn no_ghost_when_buffer_ends_in_space() {
        assert_eq!(compute_ghost("git ", "checkout"), None);
    }

    #[test]
    fn no_ghost_when_token_empty() {
        assert_eq!(compute_ghost("", "git"), None);
    }

    #[test]
    fn no_ghost_when_insertion_does_not_extend_token() {
        // Default is prefix matching: "co" is not a prefix of "checkout".
        assert_eq!(compute_ghost("git co", "checkout"), None);
    }

    #[test]
    fn no_ghost_when_fully_typed() {
        assert_eq!(compute_ghost("git checkout", "checkout"), None);
    }

    #[test]
    fn render_wraps_in_save_erase_faint_restore() {
        assert_eq!(render_seq("xyz"), "\x1b7\x1b[K\x1b[2mxyz\x1b[0m\x1b8");
    }

    #[test]
    fn clear_saves_erases_restores() {
        assert_eq!(clear_seq(), "\x1b7\x1b[K\x1b8");
    }
}
