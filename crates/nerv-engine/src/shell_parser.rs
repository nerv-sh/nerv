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

// Helpers, literal parsers, and dispatchers are wired up progressively
// across M0-4 chunks 3b–3f; the early ones go un-called until later
// chunks land. Lift the dead-code lint module-wide so each in-progress
// chunk compiles cleanly without a forest of per-fn `#[allow]`s. To
// remove once chunk 3f wires `parse()` into the full pipeline.
#![allow(dead_code)]

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
// Literal parsers (chunk 3c)
// ---------------------------------------------------------------------------

/// Construct a literal-style `Node` and derive its `inner_text`.
fn build_literal_node(
    input: &str,
    start: usize,
    end: usize,
    kind: NodeKind,
    children: Vec<Node>,
    complete: bool,
) -> Node {
    let span = start..end;
    let text = input[span.clone()].to_string();
    let inner_text = compute_inner_text(kind, &text, complete, &children);
    Node {
        kind,
        span,
        text,
        inner_text,
        complete,
        children,
        operator: None,
    }
}

/// Generic delimited-literal parser used by the five flavours below.
///
/// - `String` and `Expansion` may contain nested `$...` / `` `...` ``
///   children; for the others (`RawString`, `AnsiCString`,
///   `ArithmeticExpansion`) the body is a flat byte sequence.
/// - Backslash escapes consume the following byte, except inside
///   `RawString` where the backslash is literal.
/// - Returns a `complete: false` node when EOF arrives before the
///   closing delimiter is seen.
fn parse_delimited_literal(
    input: &str,
    start: usize,
    kind: NodeKind,
    open: &str,
    close: &str,
) -> Node {
    let body_start = start + open.len();
    let can_have_children = matches!(kind, NodeKind::String | NodeKind::Expansion);
    let in_string = matches!(kind, NodeKind::String);
    let close_bytes = close.as_bytes();

    let mut children: Vec<Node> = Vec::new();
    let mut i = body_start;
    let bytes = input.as_bytes();

    while i < input.len() {
        // Nested expansion / command-substitution child inside String / Expansion.
        if can_have_children {
            let terminators = [close.chars().next().unwrap_or('\0')];
            if let Some(child) = child_at_idx(input, i, in_string, &terminators) {
                i = child.span.end;
                children.push(child);
                continue;
            }
        }

        // Backslash escape — skip both bytes, except in raw strings.
        if bytes[i] == b'\\' && !matches!(kind, NodeKind::RawString) && i + 1 < input.len() {
            i += 2;
            continue;
        }

        // Closing delimiter?
        if i + close_bytes.len() <= input.len() && &bytes[i..i + close_bytes.len()] == close_bytes {
            return build_literal_node(input, start, i + close_bytes.len(), kind, children, true);
        }

        // Advance one char (handle UTF-8 multibyte boundaries).
        if bytes[i].is_ascii() {
            i += 1;
        } else {
            let ch = input[i..].chars().next().expect("non-empty input");
            i += ch.len_utf8();
        }
    }

    build_literal_node(input, start, input.len(), kind, children, false)
}

/// Parse `"..."` — double-quoted string with embedded expansions.
fn parse_string(input: &str, at: usize) -> Node {
    parse_delimited_literal(input, at, NodeKind::String, "\"", "\"")
}

/// Parse `'...'` — single-quoted raw string, no escape processing.
fn parse_raw_string(input: &str, at: usize) -> Node {
    parse_delimited_literal(input, at, NodeKind::RawString, "'", "'")
}

/// Parse `${...}` — parameter expansion with possible inner expansions.
fn parse_expansion(input: &str, at: usize) -> Node {
    parse_delimited_literal(input, at, NodeKind::Expansion, "${", "}")
}

/// Parse `$'...'` — ANSI-C quoted string. Escape sequences inside are
/// preserved verbatim by this parser; downstream callers may interpret.
fn parse_ansi_c(input: &str, at: usize) -> Node {
    parse_delimited_literal(input, at, NodeKind::AnsiCString, "$'", "'")
}

/// Parse `$((...))` — arithmetic expansion.
fn parse_arithmetic(input: &str, at: usize) -> Node {
    parse_delimited_literal(input, at, NodeKind::ArithmeticExpansion, "$((", "))")
}

/// Parse `$(...)` or `` `...` ``.
///
/// Chunk 3c implementation is a *shallow* skip-to-terminator — the
/// substitution body is not recursively parsed yet. Chunk 3f will swap
/// this for a call into `parse_statements` so the inner command tree
/// is built.
fn parse_command_substitution(input: &str, at: usize, term: char) -> Node {
    let body_start = at
        + if input.as_bytes().get(at) == Some(&b'`') {
            1
        } else {
            2
        };
    let term_byte = term as u8;
    let bytes = input.as_bytes();
    let mut i = body_start;
    while i < input.len() {
        if bytes[i] == term_byte {
            return build_literal_node(
                input,
                at,
                i + 1,
                NodeKind::CommandSubstitution,
                Vec::new(),
                true,
            );
        }
        i += 1;
    }
    build_literal_node(
        input,
        at,
        input.len(),
        NodeKind::CommandSubstitution,
        Vec::new(),
        false,
    )
}

/// Parse `$VAR` (`SimpleExpansion`) or a one-byte special expansion
/// (`SpecialExpansion`): `$@`, `$*`, `$?`, `$-`, `$$`, `$0`, `$_`.
///
/// Returns `None` if the `$` stands alone (no name follows — caller
/// treats it as a literal `$` byte).
fn parse_simple_expansion(input: &str, at: usize, extra_terminators: &[char]) -> Option<Node> {
    let bytes = input.as_bytes();
    debug_assert_eq!(bytes.get(at), Some(&b'$'));
    let next = *bytes.get(at + 1)?;

    // Single-byte special expansions.
    if matches!(next, b'*' | b'@' | b'?' | b'-' | b'$' | b'0' | b'_') {
        return Some(build_literal_node(
            input,
            at,
            at + 2,
            NodeKind::SpecialExpansion,
            Vec::new(),
            true,
        ));
    }

    // Simple expansion: read a name until whitespace / `$` / `\` / one
    // of `extra_terminators`.
    let stop: Vec<u8> = ['\t', ' ', '\n', '$', '\\']
        .iter()
        .chain(extra_terminators.iter())
        .map(|c| *c as u8)
        .collect();

    let mut i = at + 1;
    while i < input.len() {
        if stop.contains(&bytes[i]) {
            if i == at + 1 {
                return None;
            }
            return Some(build_literal_node(
                input,
                at,
                i,
                NodeKind::SimpleExpansion,
                Vec::new(),
                true,
            ));
        }
        i += 1;
    }
    if i == at + 1 {
        return None;
    }
    Some(build_literal_node(
        input,
        at,
        i,
        NodeKind::SimpleExpansion,
        Vec::new(),
        true,
    ))
}

/// Try to recognise a literal at byte index `at`.
///
/// Returns `None` if no literal kind begins at this position (the
/// caller then advances by one byte as a `Word` character).
fn child_at_idx(input: &str, at: usize, in_string: bool, terminators: &[char]) -> Option<Node> {
    let bytes = input.as_bytes();
    let c0 = *bytes.get(at)?;
    let c1 = bytes.get(at + 1).copied();
    let c2 = bytes.get(at + 2).copied();
    match c0 {
        b'$' => match c1 {
            Some(b'(') => Some(if c2 == Some(b'(') {
                parse_arithmetic(input, at)
            } else {
                parse_command_substitution(input, at, ')')
            }),
            Some(b'{') => Some(parse_expansion(input, at)),
            Some(b'\'') if !in_string => Some(parse_ansi_c(input, at)),
            _ => parse_simple_expansion(input, at, terminators),
        },
        b'`' => Some(parse_command_substitution(input, at, '`')),
        b'\'' if !in_string => Some(parse_raw_string(input, at)),
        b'"' if !in_string => Some(parse_string(input, at)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Entry point (chunks 3d–3f land progressively)
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

    // -- parse_string / parse_raw_string -----------------------------------

    #[test]
    fn parse_raw_string_complete_pair() {
        let n = parse_raw_string("'hello'", 0);
        assert_eq!(n.kind, NodeKind::RawString);
        assert_eq!(n.span, 0..7);
        assert!(n.complete);
        assert_eq!(n.inner_text, "hello");
    }

    #[test]
    fn parse_raw_string_unterminated() {
        let n = parse_raw_string("'hello", 0);
        assert!(!n.complete);
        assert_eq!(n.span, 0..6);
        assert_eq!(n.inner_text, "hello");
    }

    #[test]
    fn parse_string_escapes_inner_quote() {
        let n = parse_string("\"a\\\"b\"", 0);
        assert!(n.complete);
        assert_eq!(n.inner_text, "a\"b");
    }

    #[test]
    fn parse_string_with_simple_expansion_child() {
        let n = parse_string("\"$HOME\"", 0);
        assert_eq!(n.kind, NodeKind::String);
        assert!(n.complete);
        assert_eq!(n.children.len(), 1);
        assert_eq!(n.children[0].kind, NodeKind::SimpleExpansion);
        assert_eq!(n.children[0].text, "$HOME");
    }

    #[test]
    fn parse_raw_string_no_escape_inside() {
        // Single quote terminates at the first ' regardless of preceding \.
        let n = parse_raw_string("'a\\'b'", 0);
        assert!(n.complete);
        assert_eq!(n.inner_text, "a\\");
    }

    // -- parse_expansion / parse_ansi_c / parse_arithmetic -----------------

    #[test]
    fn parse_expansion_complete() {
        let n = parse_expansion("${HOME}", 0);
        assert_eq!(n.kind, NodeKind::Expansion);
        assert!(n.complete);
        assert_eq!(n.span, 0..7);
    }

    #[test]
    fn parse_expansion_unterminated() {
        let n = parse_expansion("${HOME", 0);
        assert!(!n.complete);
    }

    #[test]
    fn parse_ansi_c_strips_dollar_quote() {
        let n = parse_ansi_c("$'abc'", 0);
        assert_eq!(n.kind, NodeKind::AnsiCString);
        assert!(n.complete);
        assert_eq!(n.inner_text, "abc");
    }

    #[test]
    fn parse_arithmetic_double_paren() {
        let n = parse_arithmetic("$((1+2))", 0);
        assert_eq!(n.kind, NodeKind::ArithmeticExpansion);
        assert!(n.complete);
        assert_eq!(n.text, "$((1+2))");
    }

    // -- parse_simple_expansion --------------------------------------------

    #[test]
    fn parse_simple_expansion_variable_name() {
        let n = parse_simple_expansion("$FOO", 0, &[]).expect("some");
        assert_eq!(n.kind, NodeKind::SimpleExpansion);
        assert_eq!(n.text, "$FOO");
        assert_eq!(n.span, 0..4);
    }

    #[test]
    fn parse_simple_expansion_stops_at_whitespace() {
        let n = parse_simple_expansion("$FOO bar", 0, &[]).expect("some");
        assert_eq!(n.span, 0..4);
    }

    #[test]
    fn parse_simple_expansion_special_chars() {
        for (input, _expected_text) in [
            ("$@", "$@"),
            ("$*", "$*"),
            ("$?", "$?"),
            ("$-", "$-"),
            ("$$", "$$"),
            ("$0", "$0"),
            ("$_", "$_"),
        ] {
            let n = parse_simple_expansion(input, 0, &[]).expect("some");
            assert_eq!(n.kind, NodeKind::SpecialExpansion);
            assert_eq!(n.span, 0..2, "wrong span for {input:?}");
        }
    }

    #[test]
    fn parse_simple_expansion_bare_dollar_returns_none() {
        // `$` at EOF (no following char) — literal `$`, caller falls back.
        assert!(parse_simple_expansion("$", 0, &[]).is_none());
    }

    #[test]
    fn parse_simple_expansion_stops_at_extra_terminator() {
        // E.g., when used inside a "..." string, `"` is a terminator.
        let n = parse_simple_expansion("$FOO\"", 0, &['"']).expect("some");
        assert_eq!(n.span, 0..4);
    }

    // -- parse_command_substitution (chunk 3c stub) ------------------------

    #[test]
    fn parse_command_substitution_dollar_paren_complete() {
        let n = parse_command_substitution("$(ls)", 0, ')');
        assert_eq!(n.kind, NodeKind::CommandSubstitution);
        assert!(n.complete);
        assert_eq!(n.span, 0..5);
    }

    #[test]
    fn parse_command_substitution_backtick_complete() {
        let n = parse_command_substitution("`pwd`", 0, '`');
        assert!(n.complete);
        assert_eq!(n.span, 0..5);
    }

    #[test]
    fn parse_command_substitution_unterminated() {
        let n = parse_command_substitution("$(ls", 0, ')');
        assert!(!n.complete);
        assert_eq!(n.span, 0..4);
    }

    // -- child_at_idx dispatcher -------------------------------------------

    #[test]
    fn child_at_idx_recognises_double_quote_outside_string() {
        let n = child_at_idx("\"x\"", 0, false, &[]).expect("some");
        assert_eq!(n.kind, NodeKind::String);
    }

    #[test]
    fn child_at_idx_skips_double_quote_inside_string() {
        // We're already inside a "...", so `"` should not start another String.
        assert!(child_at_idx("\"x\"", 0, true, &[]).is_none());
    }

    #[test]
    fn child_at_idx_dispatches_arithmetic_over_command_sub() {
        let n = child_at_idx("$((1))", 0, false, &[]).expect("some");
        assert_eq!(n.kind, NodeKind::ArithmeticExpansion);
    }

    #[test]
    fn child_at_idx_dispatches_command_sub_dollar_paren() {
        let n = child_at_idx("$(ls)", 0, false, &[]).expect("some");
        assert_eq!(n.kind, NodeKind::CommandSubstitution);
    }

    #[test]
    fn child_at_idx_returns_none_on_plain_letter() {
        assert!(child_at_idx("abc", 0, false, &[]).is_none());
    }
}
