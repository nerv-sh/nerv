//! The complete set of escape sequences the PTY overlay is allowed to
//! emit — the ANSI budget from CLAUDE.md §4 / terminal-compat.md §3 in
//! one place.
//!
//! Every overlay layer (ghost text, popup box) draws using *only* these.
//! No alternate screen, no truecolor, no OSC 8/52 — keeping the contract
//! in a single module gives one audit point when reviewing that the
//! rendering paths stay inside the whitelist.

/// `ESC 7` — save cursor position (DECSC).
pub const SAVE: &str = "\x1b7";
/// `ESC 8` — restore cursor position (DECRC).
pub const RESTORE: &str = "\x1b8";
/// `ESC [ K` — erase from cursor to end of line.
pub const ERASE_EOL: &str = "\x1b[K";
/// `ESC [ 0 m` — reset all SGR attributes.
pub const RESET: &str = "\x1b[0m";
/// `ESC [ 2 m` — faint intensity (ghost text).
pub const FAINT: &str = "\x1b[2m";
/// `ESC [ 7 m` — reverse video (popup selection).
pub const REVERSE: &str = "\x1b[7m";
/// `ESC [ B` then CR — move down one row, return to column 0.
pub const DOWN_COL0: &str = "\x1b[B\r";
