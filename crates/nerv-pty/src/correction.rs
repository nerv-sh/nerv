//! Command-word typo correction for PTY mode.
//!
//! The engine answers a line whose command word matches no spec, no PATH
//! entry and no frecency name with a single row that carries a
//! [`ReplaceSpan`] over that word (`zpeh li` → `did you mean zeph`, see
//! CLAUDE.md §3 "공백 뒤 명령 단어 교정"). The ZLE widget rewrites just
//! that span and keeps the arguments. This module is the PTY half of the
//! same contract.
//!
//! Two things differ from the widget, and both are why accepting is an
//! explicit keypress here rather than the default action:
//!
//! 1. **We cannot see the shell's own name tables.** The widget asks zsh
//!    whether the word is an alias, function, builtin or reserved word
//!    before it shows the row; a PTY wrapper has no such handle. A static
//!    builtin/keyword table (below) covers the shell-owned names, but a
//!    user's alias or function is invisible — `gitp` aliased to something
//!    real can still draw a correction hint.
//! 2. **The correction sits behind the cursor.** The ghost layer only ever
//!    appends, so a correction is drawn as a faint hint and applied by
//!    rewriting the line, never silently.
//!
//! The renderer stays inside the same ANSI budget as the ghost layer —
//! this module builds no escape sequences of its own, it hands text to
//! [`crate::ghost::render_seq`].

use nerv_engine::Suggestion;

/// A pending command-word rewrite: replace the buffer's chars in
/// `start..end` with `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    start: usize,
    end: usize,
    replacement: String,
}

impl Correction {
    /// The faint text drawn after the cursor while this correction is
    /// pending. Deliberately a sentence, not a completion: nothing here
    /// extends the token under the cursor, so a bare word would read as
    /// something Right-arrow would append.
    pub fn hint(&self) -> String {
        format!("  did you mean {}", self.replacement)
    }

    /// Bytes that rewrite the line with the correction applied, assuming
    /// the cursor is at end-of-line.
    ///
    /// DEL (`0x7f`) back to the start of the word, then retype the word
    /// and everything that followed it. Deleting the tail and retyping it
    /// is not the shortest possible edit, but it is the only one that
    /// needs no cursor-motion keybinding: `backward-delete-char` is bound
    /// to DEL in zsh, bash readline and fish alike, while "move left N
    /// words" is not portable between them (and not portable between a
    /// user's emacs and vi keymaps either).
    pub fn apply_bytes(&self, buffer: &str) -> Vec<u8> {
        let chars: Vec<char> = buffer.chars().collect();
        if self.end > chars.len() {
            return Vec::new();
        }
        let mut out = vec![0x7f; chars.len() - self.start];
        out.extend(self.replacement.as_bytes());
        out.extend(chars[self.end..].iter().collect::<String>().as_bytes());
        out
    }
}

/// Rows that complete the token under the cursor — everything the ghost
/// and popup layers are allowed to see.
pub fn only_current_token(items: Vec<Suggestion>) -> Vec<Suggestion> {
    items.into_iter().filter(|s| s.replace.is_none()).collect()
}

/// Read a pending correction out of a daemon response, or `None`.
///
/// The engine returns a correction as the *only* row (see
/// `complete_in`'s miss branch), so more than one row means these are
/// ordinary completions and none of them may rewrite the line.
pub fn from_rows(buffer: &str, items: &[Suggestion]) -> Option<Correction> {
    let [row] = items else { return None };
    let span = row.replace.as_ref()?;
    let (start, end) = (span.start as usize, span.end as usize);
    let chars: Vec<char> = buffer.chars().collect();
    if start >= end || end > chars.len() {
        return None;
    }
    let word: String = chars[start..end].iter().collect();
    if word == row.insertion || is_shell_word(&word) {
        return None;
    }
    Some(Correction {
        start,
        end,
        replacement: row.insertion.clone(),
    })
}

/// Is this word owned by the shell rather than by PATH?
///
/// The daemon only sees names it can enumerate — spec stems, frecency
/// entries and PATH executables — so a builtin or keyword looks exactly
/// like a typo to it (`export` is distance 2 from the bundled `expo`
/// spec, `hash` distance 1 from `bash`). The widget asks zsh directly;
/// here the table below stands in for that answer.
///
/// It is the **union** over zsh, bash and fish rather than a per-shell
/// table: the cost of over-blocking is one missing hint for a word that
/// is a builtin somewhere, and being wrong in the other direction puts a
/// bogus rewrite in front of the user.
fn is_shell_word(word: &str) -> bool {
    SHELL_WORDS.binary_search(&word).is_ok()
}

/// Builtins and reserved words of length ≥3 (the engine offers no
/// correction below that), collected 2026-09-18 from the shells
/// themselves: `zsh -fc 'print -l ${(k)builtins} $reswords'`,
/// `bash -c 'compgen -b; compgen -k'`, `fish -c 'builtin -n'`.
/// Sorted — [`is_shell_word`] binary-searches it.
const SHELL_WORDS: &[&str] = &[
    "abbr",
    "alias",
    "and",
    "argparse",
    "autoload",
    "begin",
    "bind",
    "bindkey",
    "block",
    "break",
    "breakpoint",
    "builtin",
    "bye",
    "caller",
    "case",
    "chdir",
    "command",
    "commandline",
    "compadd",
    "comparguments",
    "compcall",
    "compctl",
    "compdescribe",
    "compfiles",
    "compgen",
    "compgroups",
    "complete",
    "compquote",
    "compset",
    "comptags",
    "comptry",
    "compvalues",
    "contains",
    "continue",
    "coproc",
    "count",
    "declare",
    "dirs",
    "disable",
    "disown",
    "done",
    "echo",
    "echotc",
    "echoti",
    "elif",
    "else",
    "emit",
    "emulate",
    "enable",
    "end",
    "esac",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fish_indent",
    "fish_key_reader",
    "float",
    "for",
    "foreach",
    "function",
    "functions",
    "getln",
    "getopts",
    "hash",
    "help",
    "history",
    "integer",
    "jobs",
    "kill",
    "let",
    "limit",
    "local",
    "log",
    "logout",
    "math",
    "nocorrect",
    "noglob",
    "not",
    "path",
    "popd",
    "print",
    "printf",
    "private",
    "pushd",
    "pushln",
    "pwd",
    "random",
    "read",
    "readonly",
    "realpath",
    "rehash",
    "repeat",
    "return",
    "sched",
    "select",
    "set",
    "set_color",
    "setopt",
    "shift",
    "shopt",
    "source",
    "status",
    "string",
    "suspend",
    "switch",
    "test",
    "then",
    "time",
    "times",
    "trap",
    "true",
    "ttyctl",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unfunction",
    "unhash",
    "unlimit",
    "unset",
    "unsetopt",
    "until",
    "vared",
    "wait",
    "whence",
    "where",
    "which",
    "while",
    "zcompile",
    "zformat",
    "zle",
    "zmodload",
    "zparseopts",
    "zregexparse",
    "zstyle",
];

#[cfg(test)]
mod tests {
    use super::*;
    use nerv_engine::ReplaceSpan;

    fn row(insertion: &str, span: Option<(u32, u32)>) -> Suggestion {
        Suggestion {
            insertion: insertion.to_string(),
            display: insertion.to_string(),
            replace: span.map(|(start, end)| ReplaceSpan { start, end }),
            ..Default::default()
        }
    }

    #[test]
    fn a_lone_span_row_is_a_correction() {
        let c = from_rows("dokcer ps", &[row("docker", Some((0, 6)))]).unwrap();
        assert_eq!(
            c.apply_bytes("dokcer ps").len(),
            "dokcer ps".len() + "docker ps".len()
        );
    }

    #[test]
    fn ordinary_rows_are_not_corrections() {
        assert!(from_rows("git che", &[row("checkout", None)]).is_none());
    }

    #[test]
    fn a_span_row_among_others_is_ignored() {
        let items = [row("docker", Some((0, 6))), row("checkout", None)];
        assert!(from_rows("dokcer ps", &items).is_none());
    }

    #[test]
    fn a_shell_builtin_is_never_corrected() {
        // `export` is distance 2 from the bundled `expo` spec; zsh owns
        // the name and the daemon cannot know that.
        assert!(from_rows("export FOO=1 ", &[row("expo", Some((0, 6)))]).is_none());
        assert!(from_rows("hash x", &[row("bash", Some((0, 4)))]).is_none());
    }

    #[test]
    fn a_span_past_the_end_of_the_line_is_ignored() {
        assert!(from_rows("dok", &[row("docker", Some((0, 6)))]).is_none());
    }

    #[test]
    fn the_span_is_read_in_characters_not_bytes() {
        // The guards read the word out of the span, so a byte index reads
        // garbage there and lets anything through. Chars 3..9 of
        // "한글 export FOO=1 " are exactly `export` — a builtin, so no
        // correction. Bytes 3..9 are the tail of 글 plus " ex".
        let items = [row("expo", Some((3, 9)))];
        assert!(from_rows("한글 export FOO=1 ", &items).is_none());
    }

    #[test]
    fn applying_after_a_multibyte_prefix_deletes_by_character() {
        // One DEL per character, not per byte: 한글 is 6 bytes and the
        // shell erases graphemes, so a byte count would eat the prefix.
        let c = from_rows("한글 dokcer ps", &[row("docker", Some((3, 9)))]).unwrap();
        assert_eq!(c.apply_bytes("한글 dokcer ps"), {
            let mut want = vec![0x7f; "dokcer ps".chars().count()];
            want.extend(b"docker ps");
            want
        });
    }

    #[test]
    fn applying_deletes_back_to_the_word_and_retypes_the_tail() {
        let c = from_rows("sudo dokcer ps -a", &[row("docker", Some((5, 11)))]).unwrap();
        let bytes = c.apply_bytes("sudo dokcer ps -a");
        assert_eq!(
            bytes.iter().filter(|b| **b == 0x7f).count(),
            "dokcer ps -a".len()
        );
        assert_eq!(
            String::from_utf8(bytes.into_iter().filter(|b| *b != 0x7f).collect()).unwrap(),
            "docker ps -a"
        );
    }

    #[test]
    fn the_hint_reads_as_a_sentence_not_as_appended_text() {
        let c = from_rows("dokcer ", &[row("docker", Some((0, 6)))]).unwrap();
        assert_eq!(c.hint(), "  did you mean docker");
    }

    #[test]
    fn only_current_token_drops_rows_that_rewrite_another_span() {
        let kept = only_current_token(vec![row("checkout", None), row("docker", Some((0, 6)))]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].insertion, "checkout");
    }

    #[test]
    fn the_builtin_table_is_sorted_so_the_lookup_can_binary_search() {
        assert!(
            SHELL_WORDS.windows(2).all(|w| w[0] < w[1]),
            "SHELL_WORDS must be sorted"
        );
    }
}
