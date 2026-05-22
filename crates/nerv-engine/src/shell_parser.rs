//! shell_parser — bash shell command-line tokenizer & parser.
//!
//! Parses an interactive shell line into a tree of nodes (program,
//! statements, commands, words, expansions, etc). Consumers walk the
//! tree to identify the token under the cursor and the surrounding
//! command context for completion.
//!
//! Grammar supported (subset of POSIX shell / bash):
//! - statement terminators: `;`, `&`, `&;`
//! - statement composition: `||`, `&&`, `|`, `|&`
//! - compound statements `{ ... }` and subshells `( ... )`
//! - assignment lists: `FOO=bar BAZ=qux cmd ...`, `arr[0]=x`, `+=`
//! - literals: word, `"..."`, `'...'` (raw), `$'...'` (ANSI-C),
//!   `${...}` expansion, `$(...)` and `` `...` `` command substitution,
//!   `$((...))` arithmetic, `$VAR`, `$@`/`$*`/`$?`/`$-`/`$$`/`$0`/`$_`
//!
//! Adapted to Rust from the TypeScript implementation in
//! `aws/amazon-q-developer-cli-autocomplete` (Apache-2.0 + MIT). The
//! published grammar (above) is factual; the Rust expression here is
//! original — idiomatic enums, slice-based indexing, and `Range<usize>`
//! spans instead of separate start/end fields.
//!
//! Status: M0-4 in progress. Public types and the `parse` entry point
//! are stable; the implementation lands in subsequent chunks.

use std::ops::Range;

/// Statement-level operators recognised by the tokenizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `;` — sequential terminator.
    Semi,
    /// `&` — background terminator.
    Amp,
    /// `&;` — background-then-sequential terminator.
    AmpSemi,
    /// `|` — pipe.
    Pipe,
    /// `|&` — pipe with stderr.
    PipeAmp,
    /// `&&` — short-circuit AND.
    And,
    /// `||` — short-circuit OR.
    Or,
}

impl Operator {
    /// Byte length of the operator literal. Always 1 or 2 (never zero);
    /// `is_empty` is intentionally omitted.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> usize {
        match self {
            Operator::Semi | Operator::Amp | Operator::Pipe => 1,
            Operator::AmpSemi | Operator::PipeAmp | Operator::And | Operator::Or => 2,
        }
    }

    /// The operator as written in shell source.
    pub fn as_str(self) -> &'static str {
        match self {
            Operator::Semi => ";",
            Operator::Amp => "&",
            Operator::AmpSemi => "&;",
            Operator::Pipe => "|",
            Operator::PipeAmp => "|&",
            Operator::And => "&&",
            Operator::Or => "||",
        }
    }
}

/// Kinds of nodes that can appear in the parse tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// Root node — wraps zero or more statements.
    Program,

    // Assignment-list cluster
    /// `FOO=bar BAZ=qux [cmd args...]`
    AssignmentList,
    /// Single `name[=|+=]value`.
    Assignment,
    /// Left-hand-side variable name in an assignment.
    VariableName,
    /// `name[index]` left-hand-side in an array assignment.
    Subscript,

    // Statement-level
    /// `{ stmts; }`
    CompoundStatement,
    /// `( stmts )`
    Subshell,
    /// `cmd arg arg ...`
    Command,
    /// `cmd1 | cmd2` (or `|&`).
    Pipeline,
    /// `cmd1 && cmd2` or `cmd1 || cmd2`.
    List,

    /// `<(cmd)` — placeholder, not yet implemented.
    ProcessSubstitution,

    // Argument-level / literal
    /// Two or more adjacent literals with no whitespace between them.
    Concatenation,
    /// Bare word (unquoted).
    Word,
    /// `"..."` — double-quoted string with embedded expansions.
    String,
    /// `${...}` — parameter expansion.
    Expansion,
    /// `$(...)` or `` `...` `` — command substitution.
    CommandSubstitution,

    // Leaf nodes
    /// `'...'` — single-quoted raw string.
    RawString,
    /// `$'...'` — ANSI-C quoted string.
    AnsiCString,
    /// `$VAR`
    SimpleExpansion,
    /// `$@`, `$*`, `$?`, `$-`, `$$`, `$0`, `$_`
    SpecialExpansion,
    /// `$((...))`
    ArithmeticExpansion,
}

/// Operator attached to `List` (composition) and `Assignment` nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeOperator {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `|`
    Pipe,
    /// `|&`
    PipeAmp,
    /// `=`
    Assign,
    /// `+=`
    AppendAssign,
}

/// A node in the parse tree.
///
/// `span` is the byte range in the original input. `text` is the
/// substring covered by `span`; `inner_text` strips outer quotes and
/// processes escapes (for `String`, `RawString`, `AnsiCString`,
/// `Word`, and `Concatenation`).
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    pub span: Range<usize>,
    pub text: String,
    pub inner_text: String,
    /// `false` when EOF arrived before the expected closing delimiter
    /// or terminator (used by completion to know a token is mid-typing).
    pub complete: bool,
    pub children: Vec<Node>,
    /// Set on `List` and `Assignment`; `None` for other kinds.
    pub operator: Option<NodeOperator>,
}

impl Node {
    /// Convenience constructor used by parsers and tests.
    pub fn leaf(kind: NodeKind, input: &str, span: Range<usize>) -> Self {
        let text = input[span.clone()].to_string();
        let inner_text = text.clone();
        Self {
            kind,
            span,
            text,
            inner_text,
            complete: true,
            children: Vec::new(),
            operator: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers (chunk 3b)
// ---------------------------------------------------------------------------

/// Return the byte index of the first non-whitespace character at or
/// after `from`. Returns `None` if only whitespace remains until EOF.
#[allow(dead_code)] // used by chunks 3c-3f + #[test] mod
pub(crate) fn next_word_idx(input: &str, from: usize) -> Option<usize> {
    if from >= input.len() {
        return None;
    }
    input[from..]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(offset, _)| from + offset)
}

/// Recognise a statement-level operator starting at byte index `at`.
///
/// Greedy: two-byte operators (`&&`, `||`, `&;`, `|&`) take priority
/// over their one-byte prefixes (`&`, `|`).
#[allow(dead_code)] // used by chunks 3c-3f + #[test] mod
pub(crate) fn parse_operator(input: &str, at: usize) -> Option<Operator> {
    let bytes = input.as_bytes();
    let first = *bytes.get(at)?;
    let second = bytes.get(at + 1).copied();
    match first {
        b'&' => match second {
            Some(b'&') => Some(Operator::And),
            Some(b';') => Some(Operator::AmpSemi),
            _ => Some(Operator::Amp),
        },
        b'|' => match second {
            Some(b'|') => Some(Operator::Or),
            Some(b'&') => Some(Operator::PipeAmp),
            _ => Some(Operator::Pipe),
        },
        b';' => Some(Operator::Semi),
        _ => None,
    }
}

/// Compute the logical inner text of a literal node.
///
/// - `Concatenation`: join of every child's `inner_text`.
/// - `String` (`"..."`): strip the leading `"` and, if `complete`,
///   the trailing `"`; process the bash-defined backslash escapes
///   for `$`, `` ` ``, `"`, `\`, and newline.
/// - `RawString` (`'...'`): strip the leading `'` and, if `complete`,
///   the trailing `'`. No escape processing inside single quotes.
/// - `AnsiCString` (`$'...'`): strip the leading `$'` and, if
///   `complete`, the trailing `'`. ANSI-C escape processing itself
///   is deferred — chunk 3b returns the raw inner bytes.
/// - `Word`: process the word-form backslash escape (skip `\`,
///   take next char literally).
/// - All other kinds: `text` returned unchanged.
#[allow(dead_code)] // used by chunks 3c-3f + #[test] mod
pub(crate) fn compute_inner_text(
    kind: NodeKind,
    text: &str,
    complete: bool,
    children: &[Node],
) -> String {
    if matches!(kind, NodeKind::Concatenation) {
        return children.iter().map(|c| c.inner_text.as_str()).collect();
    }

    let (open, close): (&str, &str) = match kind {
        NodeKind::String => ("\"", "\""),
        NodeKind::RawString => ("'", "'"),
        NodeKind::AnsiCString => ("$'", "'"),
        _ => ("", ""),
    };

    let close = if complete { close } else { "" };
    let start = open.len();
    let end = text.len().saturating_sub(close.len());
    if start > end {
        return String::new();
    }

    let body = &text[start..end];
    if matches!(kind, NodeKind::RawString) {
        return body.to_string();
    }

    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            let next = bytes.get(i + 1).copied();
            let is_word_escape = matches!(kind, NodeKind::Word) && next.is_some();
            let is_string_escape = matches!(kind, NodeKind::String)
                && matches!(next, Some(b'$' | b'`' | b'"' | b'\\' | b'\n'));
            if is_word_escape || is_string_escape {
                // Skip the backslash; take the following byte literally.
                if let Some(n) = next {
                    out.push(n as char);
                    i += 2;
                    continue;
                }
            }
        }
        // ASCII fast path; for multibyte UTF-8 push char-by-char below.
        if b.is_ascii() {
            out.push(b as char);
            i += 1;
        } else {
            // Find char boundary by decoding once.
            let ch = body[i..].chars().next().expect("non-empty body");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Entry point (chunks 3c–3f land progressively)
// ---------------------------------------------------------------------------

/// Parse a shell command-line string into a `Program`-rooted node tree.
///
/// The body still returns an empty `Program`; statement parsing arrives
/// in chunks 3c–3f. Helpers (`next_word_idx`, `parse_operator`,
/// `compute_inner_text`) are crate-internal and used by those chunks.
pub fn parse(input: &str) -> Node {
    Node {
        kind: NodeKind::Program,
        span: 0..input.len(),
        text: input.to_string(),
        inner_text: input.to_string(),
        complete: true,
        children: Vec::new(),
        operator: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_lengths_match_literals() {
        for op in [
            Operator::Semi,
            Operator::Amp,
            Operator::AmpSemi,
            Operator::Pipe,
            Operator::PipeAmp,
            Operator::And,
            Operator::Or,
        ] {
            assert_eq!(
                op.len(),
                op.as_str().len(),
                "len() vs as_str() mismatch for {op:?}"
            );
        }
    }

    #[test]
    fn parse_empty_returns_empty_program() {
        let node = parse("");
        assert_eq!(node.kind, NodeKind::Program);
        assert!(node.children.is_empty());
        assert_eq!(node.span, 0..0);
    }

    #[test]
    fn parse_preserves_input_span() {
        let node = parse("git status");
        assert_eq!(node.span, 0..10);
        assert_eq!(node.text, "git status");
    }

    #[test]
    fn leaf_word_extracts_text_from_input() {
        let node = Node::leaf(NodeKind::Word, "git status", 4..10);
        assert_eq!(node.text, "status");
        assert_eq!(node.span, 4..10);
        assert!(node.complete);
    }

    // -- next_word_idx ------------------------------------------------------

    #[test]
    fn next_word_idx_skips_leading_whitespace() {
        assert_eq!(next_word_idx("   hello", 0), Some(3));
    }

    #[test]
    fn next_word_idx_returns_self_when_already_non_ws() {
        assert_eq!(next_word_idx("git", 0), Some(0));
    }

    #[test]
    fn next_word_idx_returns_none_on_whitespace_only_tail() {
        assert_eq!(next_word_idx("git    ", 3), None);
    }

    #[test]
    fn next_word_idx_returns_none_past_eof() {
        assert_eq!(next_word_idx("ab", 10), None);
    }

    #[test]
    fn next_word_idx_handles_tabs_and_newlines() {
        assert_eq!(next_word_idx("a\n\t b", 1), Some(4));
    }

    // -- parse_operator -----------------------------------------------------

    #[test]
    fn parse_operator_recognises_all_seven_variants() {
        assert_eq!(parse_operator(";", 0), Some(Operator::Semi));
        assert_eq!(parse_operator("&", 0), Some(Operator::Amp));
        assert_eq!(parse_operator("&;", 0), Some(Operator::AmpSemi));
        assert_eq!(parse_operator("|", 0), Some(Operator::Pipe));
        assert_eq!(parse_operator("|&", 0), Some(Operator::PipeAmp));
        assert_eq!(parse_operator("&&", 0), Some(Operator::And));
        assert_eq!(parse_operator("||", 0), Some(Operator::Or));
    }

    #[test]
    fn parse_operator_prefers_two_byte_form() {
        // && must win over & when both fit.
        assert_eq!(parse_operator("&&x", 0), Some(Operator::And));
        // | followed by something other than | / & is just Pipe.
        assert_eq!(parse_operator("|x", 0), Some(Operator::Pipe));
    }

    #[test]
    fn parse_operator_returns_none_on_letters() {
        assert_eq!(parse_operator("abc", 0), None);
        assert_eq!(parse_operator("git status", 4), None);
    }

    #[test]
    fn parse_operator_returns_none_past_eof() {
        assert_eq!(parse_operator(";", 5), None);
    }

    // -- compute_inner_text -------------------------------------------------

    #[test]
    fn compute_inner_text_word_returns_text_as_is() {
        let s = compute_inner_text(NodeKind::Word, "hello", true, &[]);
        assert_eq!(s, "hello");
    }

    #[test]
    fn compute_inner_text_word_processes_backslash_escape() {
        // word: every \\ + next-char is taken literally as just next-char.
        let s = compute_inner_text(NodeKind::Word, r"a\ b", true, &[]);
        assert_eq!(s, "a b");
    }

    #[test]
    fn compute_inner_text_raw_string_strips_quotes() {
        let s = compute_inner_text(NodeKind::RawString, "'abc'", true, &[]);
        assert_eq!(s, "abc");
    }

    #[test]
    fn compute_inner_text_raw_string_keeps_backslash_literal() {
        // Single quotes never process escapes.
        let s = compute_inner_text(NodeKind::RawString, r"'a\b'", true, &[]);
        assert_eq!(s, r"a\b");
    }

    #[test]
    fn compute_inner_text_raw_string_incomplete_only_strips_opener() {
        let s = compute_inner_text(NodeKind::RawString, "'abc", false, &[]);
        assert_eq!(s, "abc");
    }

    #[test]
    fn compute_inner_text_double_string_strips_quotes() {
        let s = compute_inner_text(NodeKind::String, "\"abc\"", true, &[]);
        assert_eq!(s, "abc");
    }

    #[test]
    fn compute_inner_text_double_string_processes_escapes() {
        // \" inside "..." is just ".
        let s = compute_inner_text(NodeKind::String, "\"a\\\"b\"", true, &[]);
        assert_eq!(s, "a\"b");
    }

    #[test]
    fn compute_inner_text_double_string_keeps_unlisted_backslashes() {
        // \x is NOT one of $ ` " \ \n — backslash stays.
        let s = compute_inner_text(NodeKind::String, "\"a\\xb\"", true, &[]);
        assert_eq!(s, "a\\xb");
    }

    #[test]
    fn compute_inner_text_ansi_c_strips_dollar_quote_pair() {
        let s = compute_inner_text(NodeKind::AnsiCString, "$'abc'", true, &[]);
        assert_eq!(s, "abc");
    }

    #[test]
    fn compute_inner_text_concatenation_joins_children() {
        let parts = vec![
            Node {
                kind: NodeKind::Word,
                span: 0..3,
                text: "foo".into(),
                inner_text: "foo".into(),
                complete: true,
                children: vec![],
                operator: None,
            },
            Node {
                kind: NodeKind::RawString,
                span: 3..8,
                text: "'bar'".into(),
                inner_text: "bar".into(),
                complete: true,
                children: vec![],
                operator: None,
            },
        ];
        let s = compute_inner_text(NodeKind::Concatenation, "foo'bar'", true, &parts);
        assert_eq!(s, "foobar");
    }

    #[test]
    fn compute_inner_text_unknown_kind_returns_text() {
        let s = compute_inner_text(NodeKind::Command, "git status", true, &[]);
        assert_eq!(s, "git status");
    }
}
