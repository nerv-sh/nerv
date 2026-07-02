//! Inline completion popup for PTY mode (Phase 3b).
//!
//! Draws a rounded box of suggestions on the rows below the prompt, with
//! the selected row in reverse video and a `[k/total]` footer under a
//! divider — mirroring the M0 ZLE widget's chrome (CLAUDE.md §3). The
//! selection wraps (Tab/Down → next, Shift-Tab/Up → prev), jumps by a
//! window (PageDown/PageUp, clamped at the edges), and stays inside a
//! sliding window of at most `max_vis` rows.
//!
//! Two concerns live here, both unit-testable:
//!   1. Pure selection/window state (`next`/`prev`/`window`).
//!   2. Escape-sequence rendering, kept inside the same ANSI budget as
//!      the ghost layer — DECSC/DECRC save-restore, cursor-down (`ESC [
//!      B`), erase-line (`ESC [ K`), reverse SGR (`ESC [ 7 m`), reset.
//!      No alternate screen, no truecolor (terminal-compat §3). Box-
//!      drawing glyphs are BMP and width-1.
//!
//! Column alignment uses display width (`unicode-width`) so CJK / wide
//! glyphs don't push the right border out of true.
//!
//! Reserving the rows below the cursor is the caller's job via
//! [`reserve_seq`]: printing newlines scrolls the screen up, then a
//! relative cursor-up returns to the prompt — scroll-safe, unlike the
//! absolute save/restore used for the overlay itself.

use crate::ansi::{DOWN_COL0, ERASE_EOL, RESET, RESTORE, REVERSE, SAVE};
use unicode_width::UnicodeWidthStr;

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

    /// Number of list rows actually shown (excludes the chrome).
    pub fn visible(&self) -> usize {
        self.items.len().min(self.max_vis)
    }

    /// Total rows the overlay occupies on screen: top border + list +
    /// divider + footer + bottom border.
    pub fn rows(&self) -> usize {
        self.visible() + 4
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

    /// Jump the selection one visible window down, clamping at the last
    /// item — page keys page, they don't wrap (pager convention; the
    /// single-step next/prev keep their wrap behavior).
    pub fn page_next(&mut self) {
        self.selected = (self.selected + self.visible()).min(self.items.len() - 1);
        self.reframe();
    }

    /// Jump the selection one visible window up, clamping at the first item.
    pub fn page_prev(&mut self) {
        self.selected = self.selected.saturating_sub(self.visible());
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
        let max_offset = self.items.len().saturating_sub(vis);
        self.offset = self.offset.min(max_offset);
    }

    /// Inner content width (between the " " paddings inside the borders),
    /// driven by the widest display string and the footer, capped so the
    /// whole box fits in `cols`.
    fn content_w(&self, cols: usize) -> usize {
        let footer = format!("[{}/{}]", self.selected + 1, self.items.len());
        let widest = self
            .items
            .iter()
            .map(|i| i.display.width())
            .max()
            .unwrap_or(0)
            .max(footer.width());
        // Box = border + space + content + space + border = content + 4.
        let cap = cols.saturating_sub(4).max(1);
        widest.min(cap).max(1)
    }

    /// Escape sequence that paints the boxed popup on the reserved rows
    /// below the cursor and returns the cursor to where it started.
    pub fn render_seq(&self, cols: usize) -> String {
        let content_w = self.content_w(cols);
        let inner = content_w + 2; // one space of padding each side
        let hbar = "─".repeat(inner);

        let mut out = String::from(SAVE);

        // Top border.
        push_row(&mut out, &format!("╭{hbar}╮"));

        // List rows.
        let vis = self.visible();
        for row in 0..vis {
            let idx = self.offset + row;
            let body = pad_to(&self.items[idx].display, content_w);
            if idx == self.selected {
                push_row(&mut out, &format!("│{REVERSE} {body} {RESET}│"));
            } else {
                push_row(&mut out, &format!("│ {body} │"));
            }
        }

        // Divider + footer + bottom border.
        push_row(&mut out, &format!("├{hbar}┤"));
        let footer = format!("[{}/{}]", self.selected + 1, self.items.len());
        push_row(&mut out, &format!("│ {} │", pad_to(&footer, content_w)));
        push_row(&mut out, &format!("╰{hbar}╯"));

        out.push_str(RESTORE);
        out
    }
}

/// Append one overlay row: move down, clear, draw, all at column 0.
fn push_row(out: &mut String, content: &str) {
    out.push_str(DOWN_COL0);
    out.push_str(ERASE_EOL);
    out.push_str(content);
}

/// Truncate `s` to `width` display cells, then right-pad with spaces to
/// exactly `width` cells so the box's right border stays aligned.
fn pad_to(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    for _ in used..width {
        out.push(' ');
    }
    out
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
pub fn reserve_seq(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    format!("{}\x1b[{}A", "\n".repeat(n), n)
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
        let mut p = Popup::new(items(5), 3).unwrap();
        assert_eq!((p.offset, p.visible()), (0, 3));
        p.next();
        p.next();
        assert_eq!(p.offset, 0);
        p.next();
        assert_eq!(p.offset, 1);
        p.next();
        assert_eq!(p.offset, 2);
        p.next();
        assert_eq!((p.selected, p.offset), (0, 0));
    }

    #[test]
    fn page_next_jumps_by_window_and_clamps() {
        let mut p = Popup::new(items(10), 3).unwrap();
        p.page_next();
        assert_eq!((p.selected, p.offset), (3, 1));
        p.page_next();
        assert_eq!(p.selected, 6);
        p.page_next();
        assert_eq!(p.selected, 9);
        // At the end: clamp, no wrap.
        p.page_next();
        assert_eq!(p.selected, 9);
    }

    #[test]
    fn page_prev_jumps_by_window_and_clamps() {
        let mut p = Popup::new(items(10), 3).unwrap();
        for _ in 0..9 {
            p.next();
        }
        assert_eq!(p.selected, 9);
        p.page_prev();
        assert_eq!(p.selected, 6);
        p.page_prev();
        p.page_prev();
        assert_eq!(p.selected, 0);
        // At the start: clamp, no wrap.
        p.page_prev();
        assert_eq!((p.selected, p.offset), (0, 0));
    }

    #[test]
    fn page_on_short_list_clamps_within_bounds() {
        // List shorter than the window: page is a jump to the edge.
        let mut p = Popup::new(items(3), 10).unwrap();
        p.page_next();
        assert_eq!(p.selected, 2);
        p.page_prev();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn rows_is_visible_plus_chrome() {
        // top + list + divider + footer + bottom = visible + 4.
        assert_eq!(Popup::new(items(5), 3).unwrap().rows(), 7);
        assert_eq!(Popup::new(items(2), 10).unwrap().rows(), 6);
    }

    #[test]
    fn render_draws_box_with_selected_reverse() {
        let p = Popup::new(items(2), 10).unwrap();
        let seq = p.render_seq(80);
        assert!(seq.starts_with(SAVE));
        assert!(seq.ends_with(RESTORE));
        assert!(seq.contains('╭') && seq.contains('╮'));
        assert!(seq.contains('╰') && seq.contains('╯'));
        assert!(seq.contains('├') && seq.contains('┤'));
        // Selected row (item0) is wrapped in reverse; the footer shows.
        assert!(seq.contains(&format!("│{REVERSE} item0")));
        assert!(seq.contains("[1/2]"));
    }

    #[test]
    fn pad_to_truncates_and_pads_by_width() {
        assert_eq!(pad_to("abc", 5), "abc  ");
        assert_eq!(pad_to("abcdef", 4), "abcd");
        // Wide (CJK) chars count as 2 cells.
        assert_eq!(pad_to("가나", 5), "가나 ");
        assert_eq!(pad_to("가나", 3), "가 ");
    }

    #[test]
    fn render_box_width_fits_cols() {
        // Long display, narrow terminal → box clamped, rows still equal.
        let p = Popup::new(
            vec![
                PopupItem {
                    display: "a-very-long-display-name".into(),
                    insertion: "x".into(),
                },
                PopupItem {
                    display: "b".into(),
                    insertion: "y".into(),
                },
            ],
            10,
        )
        .unwrap();
        let seq = p.render_seq(12);
        // hbar width = content_w + 2 ≤ cols - 2; no row should exceed cols.
        for line in seq.split(DOWN_COL0).skip(1) {
            let visible = line
                .replace(ERASE_EOL, "")
                .replace(REVERSE, "")
                .replace(RESET, "")
                .replace(RESTORE, "");
            assert!(visible.width() <= 12, "row too wide: {visible:?}");
        }
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
