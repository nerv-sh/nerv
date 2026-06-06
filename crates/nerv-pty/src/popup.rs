//! Minimal inline completion popup for PTY mode (Phase 3b).
//!
//! Draws the suggestion list on the rows below the prompt, with the
//! selected row in reverse video and a `[k/total]` footer. Navigation
//! wraps (Tab/Down → next, Shift-Tab/Up → prev) and keeps the selection
//! inside a sliding window of at most `max_vis` rows — mirroring the M0
//! ZLE widget contract (CLAUDE.md §3).
//!
//! Two concerns live here, both unit-testable:
//!   1. Pure selection/window state (`next`/`prev`/`window`).
//!   2. Escape-sequence rendering, kept inside the same ANSI budget as
//!      the ghost layer — DECSC/DECRC save-restore, cursor-down (`ESC [
//!      B`), erase-line (`ESC [ K`), reverse SGR (`ESC [ 7 m`), reset.
//!      No alternate screen, no truecolor (terminal-compat §3).
//!
//! Reserving the rows below the cursor (so the overlay doesn't fight a
//! prompt sitting at the bottom of the screen) is the caller's job via
//! [`reserve_seq`]: printing newlines scrolls the screen up, then a
//! relative cursor-up returns to the prompt — scroll-safe, unlike the
//! absolute save/restore used for the overlay itself.

/// One row in the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupItem {
    /// What the user sees in the list.
    pub display: String,
    /// What gets inserted when this row is accepted.
    pub insertion: String,
}

/// Popup selection + scroll state.
#[derive(Debug, Clone)]
pub struct Popup {
    items: Vec<PopupItem>,
    selected: usize,
    offset: usize,
    max_vis: usize,
}

const SAVE: &str = "\x1b7";
const RESTORE: &str = "\x1b8";
const DOWN_COL0: &str = "\x1b[B\r"; // cursor down one row, then column 0
const ERASE_EOL: &str = "\x1b[K";
const REVERSE: &str = "\x1b[7m";
const RESET: &str = "\x1b[0m";

impl Popup {
    /// Build a popup. `max_vis` is clamped to at least 1. Returns `None`
    /// when there are fewer than two items — a single suggestion is the
    /// ghost layer's job, not a list.
    pub fn new(items: Vec<PopupItem>, max_vis: usize) -> Option<Self> {
        if items.len() < 2 {
            return None;
        }
        Some(Self {
            items,
            selected: 0,
            offset: 0,
            max_vis: max_vis.max(1),
        })
    }

    /// Number of list rows actually shown (excludes the footer).
    pub fn visible(&self) -> usize {
        self.items.len().min(self.max_vis)
    }

    /// Total rows the overlay occupies on screen (list + footer).
    pub fn rows(&self) -> usize {
        self.visible() + 1
    }

    /// The currently selected item.
    pub fn selected_item(&self) -> &PopupItem {
        &self.items[self.selected]
    }

    /// Advance the selection by one, wrapping at the end, scrolling the
    /// window so the selection stays visible.
    pub fn next(&mut self) {
        self.selected = (self.selected + 1) % self.items.len();
        self.reframe();
    }

    /// Move the selection back by one, wrapping at the start.
    pub fn prev(&mut self) {
        self.selected = (self.selected + self.items.len() - 1) % self.items.len();
        self.reframe();
    }

    /// Slide `offset` so `selected` sits within `[offset, offset+visible)`.
    fn reframe(&mut self) {
        let vis = self.visible();
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + vis {
            self.offset = self.selected + 1 - vis;
        }
        // Clamp so the window never runs past the end (e.g. after a
        // wrap from first to last).
        let max_offset = self.items.len().saturating_sub(vis);
        self.offset = self.offset.min(max_offset);
    }

    /// Escape sequence that paints the popup on the reserved rows below
    /// the cursor and returns the cursor to where it started. `cols`
    /// truncates each row so it can't wrap.
    pub fn render_seq(&self, cols: usize) -> String {
        let vis = self.visible();
        let mut out = String::from(SAVE);
        for row in 0..vis {
            let idx = self.offset + row;
            let item = &self.items[idx];
            out.push_str(DOWN_COL0);
            out.push_str(ERASE_EOL);
            let text = truncate(&item.display, cols);
            if idx == self.selected {
                out.push_str(REVERSE);
                out.push_str(&text);
                out.push_str(RESET);
            } else {
                out.push_str(&text);
            }
        }
        // Footer counter.
        out.push_str(DOWN_COL0);
        out.push_str(ERASE_EOL);
        let footer = format!("[{}/{}]", self.selected + 1, self.items.len());
        out.push_str(&truncate(&footer, cols));
        out.push_str(RESTORE);
        out
    }
}

/// Escape sequence that erases `rows` overlay rows below the cursor and
/// returns to the start. Safe to emit when nothing is drawn.
pub fn clear_seq(rows: usize) -> String {
    let mut out = String::from(SAVE);
    for _ in 0..rows {
        out.push_str(DOWN_COL0);
        out.push_str(ERASE_EOL);
    }
    out.push_str(RESTORE);
    out
}

/// Escape sequence that reserves `n` rows below the cursor without
/// corrupting cursor position: newlines scroll the screen up (creating
/// blank rows), then a relative cursor-up returns to the original line.
/// Newlines here are bare line-feeds (the PTY is in raw mode, so no
/// CR translation moves the column).
pub fn reserve_seq(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    format!("{}\x1b[{}A", "\n".repeat(n), n)
}

/// Truncate `s` to at most `cols` characters (char-wise; good enough for
/// the ASCII-dominant completion text — multibyte width refinement is a
/// later concern).
fn truncate(s: &str, cols: usize) -> String {
    if s.chars().count() <= cols {
        return s.to_string();
    }
    s.chars().take(cols).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<PopupItem> {
        (0..n)
            .map(|i| PopupItem {
                display: format!("item{i}"),
                insertion: format!("ins{i}"),
            })
            .collect()
    }

    #[test]
    fn none_for_fewer_than_two() {
        assert!(Popup::new(vec![], 10).is_none());
        assert!(Popup::new(items(1), 10).is_none());
        assert!(Popup::new(items(2), 10).is_some());
    }

    #[test]
    fn next_wraps_to_start() {
        let mut p = Popup::new(items(3), 10).unwrap();
        assert_eq!(p.selected, 0);
        p.next();
        p.next();
        assert_eq!(p.selected, 2);
        p.next();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn prev_wraps_to_end() {
        let mut p = Popup::new(items(3), 10).unwrap();
        p.prev();
        assert_eq!(p.selected, 2);
    }

    #[test]
    fn window_scrolls_to_keep_selection_visible() {
        // 5 items, only 3 visible.
        let mut p = Popup::new(items(5), 3).unwrap();
        assert_eq!((p.offset, p.visible()), (0, 3));
        p.next(); // 1
        p.next(); // 2
        assert_eq!(p.offset, 0); // still in window [0,3)
        p.next(); // 3 -> scroll
        assert_eq!(p.offset, 1); // window [1,4)
        p.next(); // 4 -> scroll
        assert_eq!(p.offset, 2); // window [2,5)
        p.next(); // wrap to 0
        assert_eq!((p.selected, p.offset), (0, 0));
    }

    #[test]
    fn rows_is_visible_plus_footer() {
        assert_eq!(Popup::new(items(5), 3).unwrap().rows(), 4);
        assert_eq!(Popup::new(items(2), 10).unwrap().rows(), 3);
    }

    #[test]
    fn render_marks_selected_with_reverse() {
        let p = Popup::new(items(2), 10).unwrap();
        let seq = p.render_seq(80);
        assert!(seq.starts_with(SAVE));
        assert!(seq.ends_with(RESTORE));
        // Selected row (item0) is wrapped in reverse; item1 is not.
        assert!(seq.contains(&format!("{REVERSE}item0{RESET}")));
        assert!(seq.contains("item1"));
        assert!(seq.contains("[1/2]"));
    }

    #[test]
    fn render_truncates_to_cols() {
        let p = Popup::new(
            vec![
                PopupItem {
                    display: "abcdefghij".into(),
                    insertion: "x".into(),
                },
                PopupItem {
                    display: "k".into(),
                    insertion: "y".into(),
                },
            ],
            10,
        )
        .unwrap();
        let seq = p.render_seq(4);
        assert!(seq.contains("abcd"));
        assert!(!seq.contains("abcde"));
    }

    #[test]
    fn reserve_seq_scrolls_then_returns() {
        assert_eq!(reserve_seq(0), "");
        assert_eq!(reserve_seq(3), "\n\n\n\x1b[3A");
    }

    #[test]
    fn clear_seq_wraps_in_save_restore() {
        let seq = clear_seq(2);
        assert!(seq.starts_with(SAVE));
        assert!(seq.ends_with(RESTORE));
        assert_eq!(seq.matches(ERASE_EOL).count(), 2);
    }
}
