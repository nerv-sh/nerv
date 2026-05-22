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

/// Parse `$(...)` or `` `...` `` — recursively parses the inner
/// command tree via `parse_statements` so the substitution body
/// shows up as proper Command/Pipeline/List children.
fn parse_command_substitution(input: &str, at: usize, term: char) -> Node {
    let body_start = at
        + if input.as_bytes().get(at) == Some(&b'`') {
            1
        } else {
            2
        };
    let (children, terminator_idx) = parse_statements(input, body_start, Some(term), false);
    let terminated = terminator_idx.is_some();
    let end = if let Some(t) = terminator_idx {
        t + 1
    } else {
        input.len()
    };
    let has_children = !children.is_empty();
    build_literal_node(
        input,
        at,
        end,
        NodeKind::CommandSubstitution,
        children,
        terminated && has_children,
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
// Command + concatenation (chunk 3d)
// ---------------------------------------------------------------------------

/// Promote any pending bare-word run into the current argument's
/// child list. No-op when no word is in progress.
fn close_word(
    input: &str,
    word_start: &mut Option<usize>,
    argument_children: &mut Vec<Node>,
    end_idx: usize,
) {
    if let Some(ws) = word_start.take() {
        if ws < end_idx {
            argument_children.push(build_literal_node(
                input,
                ws,
                end_idx,
                NodeKind::Word,
                Vec::new(),
                true,
            ));
        }
    }
}

/// Finalise the current argument: close any open word, then wrap the
/// accumulated children into a single `Word` / literal node when there
/// is one, or a `Concatenation` node when adjacent literals make up
/// a single shell argument (e.g. `foo"bar"`).
fn close_argument(
    input: &str,
    args: &mut Vec<Node>,
    argument_children: &mut Vec<Node>,
    word_start: &mut Option<usize>,
    end_idx: usize,
) {
    close_word(input, word_start, argument_children, end_idx);
    if argument_children.is_empty() {
        return;
    }
    let argument = if argument_children.len() == 1 {
        argument_children.pop().expect("len == 1")
    } else {
        let start_idx = argument_children.first().expect("non-empty").span.start;
        let last = argument_children.last().expect("non-empty");
        let end_span = last.span.end;
        let complete = last.complete;
        let children: Vec<Node> = std::mem::take(argument_children);
        build_literal_node(
            input,
            start_idx,
            end_span,
            NodeKind::Concatenation,
            children,
            complete,
        )
    };
    args.push(argument);
}

/// Parse a sequence of shell arguments — bare words, quoted strings,
/// expansions, command substitutions, or concatenations thereof — until
/// an operator is seen, the `terminal_char` is reached, or EOF.
///
/// Returns the argument list and the byte index where parsing stopped.
fn parse_concatenation_or_literals(
    input: &str,
    start: usize,
    terminal_char: Option<char>,
) -> (Vec<Node>, usize) {
    let mut args: Vec<Node> = Vec::new();
    let mut argument_children: Vec<Node> = Vec::new();
    let mut word_start: Option<usize> = None;

    // Terminator set used by nested literal parsers (e.g. parse_string)
    // so they know when to stop probing for more characters.
    let mut child_terminators: Vec<char> = vec!['&', '|', ';', '\n', '\'', '"', '`'];
    if let Some(tc) = terminal_char {
        child_terminators.push(tc);
    }

    let bytes = input.as_bytes();
    let mut i = start;
    while i < input.len() {
        let c = bytes[i];

        if parse_operator(input, i).is_some() || Some(c as char) == terminal_char {
            break;
        }

        if let Some(child) = child_at_idx(input, i, false, &child_terminators) {
            close_word(input, &mut word_start, &mut argument_children, i);
            i = child.span.end;
            argument_children.push(child);
            continue;
        }

        if c == b' ' || c == b'\t' {
            close_argument(input, &mut args, &mut argument_children, &mut word_start, i);
            i += 1;
            continue;
        }

        // Backslash escape: keep both the `\` and the escaped byte in
        // the current word; `compute_inner_text` will drop the `\`.
        if c == b'\\' {
            if word_start.is_none() {
                word_start = Some(i);
            }
            // Skip the backslash; then skip the escaped char (one UTF-8
            // scalar). Bail safely on EOF after a lone backslash.
            i += 1;
            if i >= input.len() {
                break;
            }
            let step = if bytes[i].is_ascii() {
                1
            } else {
                input[i..]
                    .chars()
                    .next()
                    .map(|ch| ch.len_utf8())
                    .unwrap_or(1)
            };
            i += step;
            continue;
        }

        if word_start.is_none() {
            word_start = Some(i);
        }

        let step = if bytes[i].is_ascii() {
            1
        } else {
            input[i..]
                .chars()
                .next()
                .map(|ch| ch.len_utf8())
                .unwrap_or(1)
        };
        i += step;
    }

    close_argument(input, &mut args, &mut argument_children, &mut word_start, i);
    (args, i)
}

/// Parse a single command (sequence of argument words / literals).
///
/// Leading whitespace is skipped. With no actual arguments, the command
/// node spans to EOF and is marked incomplete so callers can recognise
/// "user is still typing the first token".
fn parse_command(input: &str, at: usize, terminal_char: Option<char>) -> Node {
    let start_idx = next_word_idx(input, at).unwrap_or(at).max(at);
    let (children, end_idx) = parse_concatenation_or_literals(input, start_idx, terminal_char);
    let has_children = !children.is_empty();
    let end = if has_children { end_idx } else { input.len() };
    build_literal_node(
        input,
        start_idx,
        end,
        NodeKind::Command,
        children,
        has_children,
    )
}

// ---------------------------------------------------------------------------
// Assignment lists (chunk 3e)
// ---------------------------------------------------------------------------

/// Quick LHS check: does the prefix of `s` look like a shell variable
/// assignment? Recognises `NAME=`, `NAME+=`, and `NAME[index]=` /
/// `NAME[index]+=` forms. Pure ASCII byte check — no regex dep.
fn looks_like_assignment(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut saw_lhs_char = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'[' || b == b']' {
            saw_lhs_char = true;
            i += 1;
        } else {
            break;
        }
    }
    if !saw_lhs_char {
        return false;
    }
    if i < bytes.len() && bytes[i] == b'+' {
        i += 1;
    }
    i < bytes.len() && bytes[i] == b'='
}

/// Parse a single `NAME=value` / `NAME+=value` / `NAME[idx]=value` /
/// `NAME[idx]+=value` assignment beginning at `start`. Caller must
/// have already confirmed via `looks_like_assignment`.
fn parse_assignment_node(input: &str, start: usize) -> Node {
    let bytes = input.as_bytes();
    let equals_idx = start
        + bytes[start..]
            .iter()
            .position(|&b| b == b'=')
            .expect("looks_like_assignment guaranteed `=`");

    let (operator, lhs_end) = if equals_idx > start && bytes[equals_idx - 1] == b'+' {
        (NodeOperator::AppendAssign, equals_idx - 1)
    } else {
        (NodeOperator::Assign, equals_idx)
    };

    // First `[` within the LHS — separates plain `NAME` from `NAME[idx]`.
    let first_bracket = bytes[start..lhs_end]
        .iter()
        .position(|&b| b == b'[')
        .map(|p| start + p);

    let variable_name = build_literal_node(
        input,
        start,
        first_bracket.unwrap_or(lhs_end),
        NodeKind::VariableName,
        Vec::new(),
        true,
    );

    let name_node = if let Some(bracket_idx) = first_bracket {
        // arr[index]  — index is a Word; subscript span includes `]`.
        let index_node = build_literal_node(
            input,
            bracket_idx + 1,
            lhs_end - 1,
            NodeKind::Word,
            Vec::new(),
            true,
        );
        let subscript_end = lhs_end; // `]` is at lhs_end - 1; span ends after it
        build_literal_node(
            input,
            start,
            subscript_end,
            NodeKind::Subscript,
            vec![variable_name, index_node],
            true,
        )
    } else {
        variable_name
    };

    // Parse the right-hand value up to the next space (next assignment
    // or command token starts after the space).
    let (value_parts, end_idx) = parse_concatenation_or_literals(input, equals_idx + 1, Some(' '));

    let complete = value_parts.last().is_none_or(|c| c.complete);

    let mut children = Vec::with_capacity(1 + value_parts.len());
    children.push(name_node);
    children.extend(value_parts);

    Node {
        kind: NodeKind::Assignment,
        span: start..end_idx,
        text: input[start..end_idx].to_string(),
        inner_text: input[start..end_idx].to_string(),
        complete,
        children,
        operator: Some(operator),
    }
}

/// Parse zero or more sequential assignments starting at `start`,
/// separated by whitespace. Stops at the first token that doesn't
/// look like an assignment.
fn parse_assignments(input: &str, start: usize) -> Vec<Node> {
    let mut assignments: Vec<Node> = Vec::new();
    let mut cursor = start;
    while cursor < input.len() {
        let Some(token_start) = next_word_idx(input, cursor) else {
            break;
        };
        if !looks_like_assignment(&input[token_start..]) {
            break;
        }
        let node = parse_assignment_node(input, token_start);
        cursor = node.span.end;
        assignments.push(node);
    }
    assignments
}

/// Parse either an `AssignmentList` (one or more `FOO=bar` prefixed
/// by zero or more whitespace), optionally followed by a `Command`,
/// or fall through to a plain `Command` when no leading assignments
/// are present.
fn parse_assignment_list_or_command(
    input: &str,
    start: usize,
    terminal_char: Option<char>,
) -> Node {
    let assignments = parse_assignments(input, start);
    if assignments.is_empty() {
        return parse_command(input, start, terminal_char);
    }

    let last = assignments.last().expect("non-empty");
    let after_last = last.span.end;
    let next_op = next_word_idx(input, after_last).and_then(|idx| parse_operator(input, idx));

    // Append a Command iff the last assignment is complete, no operator
    // separates it from the next token, and there's actually more input.
    let mut command: Option<Node> = None;
    if next_op.is_none() && last.complete && after_last != input.len() {
        let cmd = parse_command(input, after_last, terminal_char);
        if !cmd.children.is_empty() {
            command = Some(cmd);
        }
    }

    let end = command.as_ref().map(|c| c.span.end).unwrap_or(after_last);
    let mut children = assignments;
    if let Some(cmd) = command {
        children.push(cmd);
    }
    let complete = children.last().is_none_or(|c| c.complete);

    Node {
        kind: NodeKind::AssignmentList,
        span: start..end,
        text: input[start..end].to_string(),
        inner_text: input[start..end].to_string(),
        complete,
        children,
        operator: None,
    }
}

// ---------------------------------------------------------------------------
// Statements & program entry (chunk 3f)
// ---------------------------------------------------------------------------

/// Compose `lhs` and `rhs` into a single `kind` node. When `rhs` is
/// already the same `kind` (nested pipeline / list), prepend `lhs` to
/// its children instead of producing a deeper tree.
fn reduce_statements(input: &str, lhs: Node, rhs: Node, kind: NodeKind) -> Node {
    let start = lhs.span.start;
    let end = rhs.span.end;
    let complete = lhs.complete && rhs.complete;
    let children = if rhs.kind == kind {
        let mut v = Vec::with_capacity(rhs.children.len() + 1);
        v.push(lhs);
        v.extend(rhs.children);
        v
    } else {
        vec![lhs, rhs]
    };
    Node {
        kind,
        span: start..end,
        text: input[start..end].to_string(),
        inner_text: input[start..end].to_string(),
        complete,
        children,
        operator: None,
    }
}

/// Parse a `;` / `&` / `&;`-separated sequence of statements.
///
/// Stops at `terminal_char` (e.g. `)` or `` ` `` for command
/// substitution, `}` for compound statements) or EOF. When
/// `must_terminate` is true (compound statement context) the inner
/// parse_statement call does not stop at `terminal_char` — the
/// terminator is observed only after each statement, in this loop.
///
/// Returns the statement list and an optional byte index pointing
/// at the seen `terminal_char` (`None` when EOF reached first).
fn parse_statements(
    input: &str,
    start: usize,
    terminal_char: Option<char>,
    must_terminate: bool,
) -> (Vec<Node>, Option<usize>) {
    let mut statements: Vec<Node> = Vec::new();
    let bytes = input.as_bytes();
    let mut i = start;

    while i < input.len() {
        let stmt_terminal = if must_terminate { None } else { terminal_char };
        let mut statement = parse_statement(input, i, stmt_terminal);

        let op_idx = next_word_idx(input, statement.span.end);

        // Did the next non-whitespace land on our terminal_char?
        if !must_terminate {
            if let (Some(idx), Some(tc)) = (op_idx, terminal_char) {
                if bytes.get(idx).copied() == Some(tc as u8) {
                    statements.push(statement);
                    return (statements, Some(idx));
                }
            }
        }

        let Some(op_idx) = op_idx else {
            statements.push(statement);
            return (statements, None);
        };

        let op = parse_operator(input, op_idx);
        if let Some(op) = op {
            // Statement terminator (; & &;) or composition (we only
            // step over the first three here; the others are inner-
            // statement operators consumed inside parse_statement).
            i = op_idx + op.len();
            statements.push(statement);

            // Look ahead for the terminal_char immediately after the
            // terminator — `cmd;)` should not invent an empty statement.
            if let (Some(next_idx), Some(tc)) = (next_word_idx(input, i), terminal_char) {
                if bytes.get(next_idx).copied() == Some(tc as u8) {
                    return (statements, Some(next_idx));
                }
            }
        } else {
            // Token sitting where a terminator would go. Treat
            // assignment-lists as still potentially complete (they
            // don't need terminators); flag other statements.
            if !matches!(statement.kind, NodeKind::AssignmentList) {
                statement.complete = false;
            }
            statements.push(statement);
            i = op_idx;
        }
    }

    (statements, None)
}

/// Parse a single statement: compound `{ ... }`, subshell `( ... )`,
/// or assignment-list / command optionally combined with the next
/// statement via `&&`, `||`, `|`, or `|&`.
fn parse_statement(input: &str, idx: usize, terminal_char: Option<char>) -> Node {
    let i = next_word_idx(input, idx).unwrap_or(idx);
    let bytes = input.as_bytes();
    let first = bytes.get(i).copied();

    let statement = if matches!(first, Some(b'{') | Some(b'(')) {
        let is_compound = first == Some(b'{');
        let end_char = if is_compound { '}' } else { ')' };
        let (children, terminator_idx) =
            parse_statements(input, i + 1, Some(end_char), is_compound);
        let has_children = !children.is_empty();
        let terminated = terminator_idx.is_some();
        let end_idx = if let Some(t) = terminator_idx {
            t + 1
        } else if has_children {
            children.last().expect("non-empty").span.end
        } else {
            input.len()
        };
        let kind = if is_compound {
            NodeKind::CompoundStatement
        } else {
            NodeKind::Subshell
        };
        Node {
            kind,
            span: i..end_idx,
            text: input[i..end_idx].to_string(),
            inner_text: input[i..end_idx].to_string(),
            complete: terminated && has_children,
            children,
            operator: None,
        }
    } else {
        parse_assignment_list_or_command(input, i, terminal_char)
    };

    // Is there a composition operator (| |& && ||) after the statement?
    let after_idx = next_word_idx(input, statement.span.end);
    let op = after_idx.and_then(|idx| parse_operator(input, idx));

    let stop_here = match op {
        None => true,
        // Statement terminators (`;`/`&`/`&;`) are consumed by
        // parse_statements, not here.
        Some(Operator::Semi | Operator::Amp | Operator::AmpSemi) => true,
        _ => after_idx
            .and_then(|idx| bytes.get(idx).copied())
            .zip(terminal_char)
            .is_some_and(|(b, tc)| b == tc as u8),
    };

    if stop_here {
        return statement;
    }

    // Composition: parse the right-hand statement and fold.
    let op = op.expect("checked Some via stop_here branch");
    let op_idx = after_idx.expect("checked Some via op match");
    let rhs = parse_statement(input, op_idx + op.len(), terminal_char);

    match op {
        Operator::And | Operator::Or => reduce_statements(input, statement, rhs, NodeKind::List),
        Operator::Pipe | Operator::PipeAmp => {
            // Special case: `cmd1 | cmd2 && cmd3`. The rhs is a List
            // whose first child is `cmd2`; we need to fold lhs+cmd2
            // into a Pipeline as the new first child of the List.
            if rhs.kind == NodeKind::List {
                let mut other = rhs.children;
                let first_child = other.remove(0);
                let new_first =
                    reduce_statements(input, statement, first_child, NodeKind::Pipeline);
                let start = new_first.span.start;
                let end = rhs.span.end;
                let mut all = Vec::with_capacity(1 + other.len());
                all.push(new_first);
                all.extend(other);
                let complete = all.iter().all(|c| c.complete);
                Node {
                    kind: NodeKind::List,
                    span: start..end,
                    text: input[start..end].to_string(),
                    inner_text: input[start..end].to_string(),
                    complete,
                    children: all,
                    operator: None,
                }
            } else {
                reduce_statements(input, statement, rhs, NodeKind::Pipeline)
            }
        }
        // Statement terminators are handled by stop_here above.
        Operator::Semi | Operator::Amp | Operator::AmpSemi => statement,
    }
}

/// Parse a full shell command-line string into a `Program`-rooted
/// node tree. The program contains the parsed statements as
/// children; each statement may itself be a Command, AssignmentList,
/// CompoundStatement, Subshell, Pipeline, or List.
pub fn parse(input: &str) -> Node {
    let (children, _) = parse_statements(input, 0, None, false);
    let complete = children.iter().all(|c| c.complete);
    Node {
        kind: NodeKind::Program,
        span: 0..input.len(),
        text: input.to_string(),
        inner_text: input.to_string(),
        complete,
        children,
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
        // Now wired: a non-empty program emits a Command child.
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].kind, NodeKind::Command);
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

    // -- parse_command / parse_concatenation_or_literals --------------------

    #[test]
    fn parse_command_single_word() {
        let n = parse_command("git", 0, None);
        assert_eq!(n.kind, NodeKind::Command);
        assert!(n.complete);
        assert_eq!(n.span, 0..3);
        assert_eq!(n.children.len(), 1);
        assert_eq!(n.children[0].kind, NodeKind::Word);
        assert_eq!(n.children[0].text, "git");
    }

    #[test]
    fn parse_command_two_words() {
        let n = parse_command("git status", 0, None);
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].text, "git");
        assert_eq!(n.children[1].text, "status");
    }

    #[test]
    fn parse_command_skips_leading_whitespace() {
        let n = parse_command("   git", 0, None);
        assert_eq!(n.span.start, 3);
        assert_eq!(n.children.len(), 1);
        assert_eq!(n.children[0].text, "git");
    }

    #[test]
    fn parse_command_empty_input_is_incomplete() {
        let n = parse_command("", 0, None);
        assert!(!n.complete);
        assert!(n.children.is_empty());
    }

    #[test]
    fn parse_command_only_whitespace_is_incomplete() {
        let n = parse_command("   ", 0, None);
        assert!(!n.complete);
        assert!(n.children.is_empty());
    }

    #[test]
    fn parse_command_stops_at_operator() {
        // semicolon is an operator → terminates the command.
        let n = parse_command("git ; echo", 0, None);
        assert_eq!(n.children.len(), 1);
        assert_eq!(n.children[0].text, "git");
        assert_eq!(n.span.end, 4);
    }

    #[test]
    fn parse_command_stops_at_terminal_char() {
        // Used by command-substitution: `$(ls foo)` → terminal char ')'.
        let n = parse_command("ls foo)", 0, Some(')'));
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].text, "ls");
        assert_eq!(n.children[1].text, "foo");
    }

    #[test]
    fn parse_command_with_double_quoted_arg() {
        let n = parse_command("git \"hello world\"", 0, None);
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].kind, NodeKind::Word);
        assert_eq!(n.children[0].text, "git");
        assert_eq!(n.children[1].kind, NodeKind::String);
        assert_eq!(n.children[1].inner_text, "hello world");
    }

    #[test]
    fn parse_command_concatenates_adjacent_literals() {
        // `foo"bar"` is a single shell argument: Word + String concatenated.
        let n = parse_command("foo\"bar\"", 0, None);
        assert_eq!(n.children.len(), 1);
        let arg = &n.children[0];
        assert_eq!(arg.kind, NodeKind::Concatenation);
        assert_eq!(arg.children.len(), 2);
        assert_eq!(arg.children[0].kind, NodeKind::Word);
        assert_eq!(arg.children[0].text, "foo");
        assert_eq!(arg.children[1].kind, NodeKind::String);
        assert_eq!(arg.children[1].text, "\"bar\"");
        assert_eq!(arg.inner_text, "foobar");
    }

    #[test]
    fn parse_command_concatenates_three_parts() {
        // pre"mid"post — three adjacent literals.
        let n = parse_command("pre\"mid\"post", 0, None);
        assert_eq!(n.children.len(), 1);
        let arg = &n.children[0];
        assert_eq!(arg.kind, NodeKind::Concatenation);
        assert_eq!(arg.children.len(), 3);
        assert_eq!(arg.inner_text, "premidpost");
    }

    #[test]
    fn parse_command_backslash_escapes_space() {
        // `echo a\ b` — one Word arg whose inner_text is "a b".
        let n = parse_command("echo a\\ b", 0, None);
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].text, "echo");
        assert_eq!(n.children[1].kind, NodeKind::Word);
        assert_eq!(n.children[1].text, "a\\ b");
        assert_eq!(n.children[1].inner_text, "a b");
    }

    #[test]
    fn parse_command_trailing_backslash_is_safe() {
        // Lone trailing `\` (no escaped char) must not panic / overflow.
        let n = parse_command("echo \\", 0, None);
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].text, "echo");
        assert_eq!(n.children[1].text, "\\");
    }

    #[test]
    fn parse_command_word_with_expansion_inside_is_word_plus_expansion() {
        // `prefix$HOME` → Concatenation of Word + SimpleExpansion.
        let n = parse_command("prefix$HOME", 0, None);
        assert_eq!(n.children.len(), 1);
        let arg = &n.children[0];
        assert_eq!(arg.kind, NodeKind::Concatenation);
        assert_eq!(arg.children.len(), 2);
        assert_eq!(arg.children[0].kind, NodeKind::Word);
        assert_eq!(arg.children[0].text, "prefix");
        assert_eq!(arg.children[1].kind, NodeKind::SimpleExpansion);
        assert_eq!(arg.children[1].text, "$HOME");
    }

    #[test]
    fn parse_command_handles_multibyte_utf8_in_word() {
        // Korean characters should pass through unharmed.
        let n = parse_command("echo 한글", 0, None);
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[1].text, "한글");
    }

    // -- looks_like_assignment ---------------------------------------------

    #[test]
    fn looks_like_assignment_simple_equals() {
        assert!(looks_like_assignment("FOO=bar"));
    }

    #[test]
    fn looks_like_assignment_append_equals() {
        assert!(looks_like_assignment("PATH+=:/bin"));
    }

    #[test]
    fn looks_like_assignment_subscript() {
        assert!(looks_like_assignment("arr[0]=x"));
    }

    #[test]
    fn looks_like_assignment_rejects_plain_command() {
        assert!(!looks_like_assignment("git status"));
    }

    #[test]
    fn looks_like_assignment_rejects_bare_name() {
        assert!(!looks_like_assignment("FOO"));
    }

    #[test]
    fn looks_like_assignment_rejects_leading_equals() {
        assert!(!looks_like_assignment("=value"));
    }

    // -- parse_assignment_node ---------------------------------------------

    #[test]
    fn parse_assignment_simple() {
        let n = parse_assignment_node("FOO=bar", 0);
        assert_eq!(n.kind, NodeKind::Assignment);
        assert_eq!(n.operator, Some(NodeOperator::Assign));
        assert_eq!(n.span, 0..7);
        // children[0] = VariableName, children[1..] = value parts.
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].kind, NodeKind::VariableName);
        assert_eq!(n.children[0].text, "FOO");
        assert_eq!(n.children[1].kind, NodeKind::Word);
        assert_eq!(n.children[1].text, "bar");
    }

    #[test]
    fn parse_assignment_append() {
        let n = parse_assignment_node("PATH+=:/bin", 0);
        assert_eq!(n.operator, Some(NodeOperator::AppendAssign));
        assert_eq!(n.children[0].kind, NodeKind::VariableName);
        assert_eq!(n.children[0].text, "PATH");
        assert_eq!(n.children[1].text, ":/bin");
    }

    #[test]
    fn parse_assignment_with_subscript() {
        let n = parse_assignment_node("arr[0]=x", 0);
        assert_eq!(n.operator, Some(NodeOperator::Assign));
        assert_eq!(n.children[0].kind, NodeKind::Subscript);
        // Subscript children: [VariableName "arr", Word "0"]
        let subscript = &n.children[0];
        assert_eq!(subscript.children.len(), 2);
        assert_eq!(subscript.children[0].kind, NodeKind::VariableName);
        assert_eq!(subscript.children[0].text, "arr");
        assert_eq!(subscript.children[1].kind, NodeKind::Word);
        assert_eq!(subscript.children[1].text, "0");
    }

    #[test]
    fn parse_assignment_with_quoted_value() {
        let n = parse_assignment_node("MSG=\"hello world\"", 0);
        assert_eq!(n.children.len(), 2);
        assert_eq!(n.children[0].text, "MSG");
        assert_eq!(n.children[1].kind, NodeKind::String);
        assert_eq!(n.children[1].inner_text, "hello world");
    }

    // -- parse_assignments (list) ------------------------------------------

    #[test]
    fn parse_assignments_zero() {
        assert!(parse_assignments("git status", 0).is_empty());
    }

    #[test]
    fn parse_assignments_one() {
        let list = parse_assignments("FOO=bar", 0);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn parse_assignments_many() {
        let list = parse_assignments("FOO=bar BAZ=qux QUUX=quux", 0);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].children[0].text, "FOO");
        assert_eq!(list[1].children[0].text, "BAZ");
        assert_eq!(list[2].children[0].text, "QUUX");
    }

    #[test]
    fn parse_assignments_stops_at_non_assignment() {
        // FOO=bar then a plain word — stop after the assignment.
        let list = parse_assignments("FOO=bar cmd", 0);
        assert_eq!(list.len(), 1);
    }

    // -- parse_assignment_list_or_command ----------------------------------

    #[test]
    fn assignment_list_only_assignments() {
        let n = parse_assignment_list_or_command("FOO=bar BAZ=qux", 0, None);
        assert_eq!(n.kind, NodeKind::AssignmentList);
        assert_eq!(n.children.len(), 2);
    }

    #[test]
    fn assignment_list_with_trailing_command() {
        let n = parse_assignment_list_or_command("FOO=bar cmd arg", 0, None);
        assert_eq!(n.kind, NodeKind::AssignmentList);
        assert_eq!(n.children.len(), 2);
        // children = [Assignment, Command]
        assert_eq!(n.children[0].kind, NodeKind::Assignment);
        assert_eq!(n.children[1].kind, NodeKind::Command);
        assert_eq!(n.children[1].children.len(), 2);
        assert_eq!(n.children[1].children[0].text, "cmd");
        assert_eq!(n.children[1].children[1].text, "arg");
    }

    #[test]
    fn assignment_list_falls_through_to_command_when_no_assignments() {
        let n = parse_assignment_list_or_command("git status", 0, None);
        assert_eq!(n.kind, NodeKind::Command);
    }

    #[test]
    fn assignment_list_empty_input_falls_through_to_incomplete_command() {
        let n = parse_assignment_list_or_command("", 0, None);
        assert_eq!(n.kind, NodeKind::Command);
        assert!(!n.complete);
    }

    // -- parse (statements + composition) -----------------------------------

    #[test]
    fn parse_single_command_yields_one_command_statement() {
        let p = parse("git status");
        assert_eq!(p.kind, NodeKind::Program);
        assert_eq!(p.children.len(), 1);
        assert_eq!(p.children[0].kind, NodeKind::Command);
        assert_eq!(p.children[0].children.len(), 2);
    }

    #[test]
    fn parse_semicolon_sequence_yields_three_statements() {
        let p = parse("a; b; c");
        assert_eq!(p.children.len(), 3);
        for child in &p.children {
            assert_eq!(child.kind, NodeKind::Command);
            assert_eq!(child.children.len(), 1);
        }
    }

    #[test]
    fn parse_pipeline_two_commands() {
        let p = parse("a | b");
        assert_eq!(p.children.len(), 1);
        assert_eq!(p.children[0].kind, NodeKind::Pipeline);
        assert_eq!(p.children[0].children.len(), 2);
    }

    #[test]
    fn parse_pipeline_three_commands_flattens() {
        // a | b | c → single Pipeline with 3 children (not nested).
        let p = parse("a | b | c");
        assert_eq!(p.children.len(), 1);
        let pipe = &p.children[0];
        assert_eq!(pipe.kind, NodeKind::Pipeline);
        assert_eq!(pipe.children.len(), 3);
    }

    #[test]
    fn parse_list_and_two_commands() {
        let p = parse("a && b");
        assert_eq!(p.children.len(), 1);
        assert_eq!(p.children[0].kind, NodeKind::List);
        assert_eq!(p.children[0].children.len(), 2);
    }

    #[test]
    fn parse_list_or_two_commands() {
        let p = parse("a || b");
        assert_eq!(p.children.len(), 1);
        assert_eq!(p.children[0].kind, NodeKind::List);
    }

    #[test]
    fn parse_mixed_pipe_and_and() {
        // a | b && c → List[Pipeline[a, b], c]
        let p = parse("a | b && c");
        assert_eq!(p.children.len(), 1);
        let list = &p.children[0];
        assert_eq!(list.kind, NodeKind::List);
        assert_eq!(list.children.len(), 2);
        assert_eq!(list.children[0].kind, NodeKind::Pipeline);
        assert_eq!(list.children[0].children.len(), 2);
        assert_eq!(list.children[1].kind, NodeKind::Command);
    }

    #[test]
    fn parse_compound_statement() {
        let p = parse("{ a; b; }");
        assert_eq!(p.children.len(), 1);
        let cs = &p.children[0];
        assert_eq!(cs.kind, NodeKind::CompoundStatement);
        assert_eq!(cs.children.len(), 2);
        assert!(cs.complete);
    }

    #[test]
    fn parse_subshell() {
        let p = parse("( ls )");
        assert_eq!(p.children.len(), 1);
        let ss = &p.children[0];
        assert_eq!(ss.kind, NodeKind::Subshell);
        assert_eq!(ss.children.len(), 1);
        assert!(ss.complete);
    }

    #[test]
    fn parse_compound_unterminated_is_incomplete() {
        let p = parse("{ a;");
        let cs = &p.children[0];
        assert_eq!(cs.kind, NodeKind::CompoundStatement);
        assert!(!cs.complete);
    }

    #[test]
    fn parse_subshell_unterminated_is_incomplete() {
        let p = parse("( a");
        let ss = &p.children[0];
        assert_eq!(ss.kind, NodeKind::Subshell);
        assert!(!ss.complete);
    }

    #[test]
    fn parse_command_substitution_now_recurses() {
        // After chunk 3f the body of $() is parsed via parse_statements.
        let p = parse("echo $(ls foo)");
        assert_eq!(p.children.len(), 1);
        let cmd = &p.children[0];
        assert_eq!(cmd.kind, NodeKind::Command);
        // children = [echo, $(ls foo)]
        assert_eq!(cmd.children.len(), 2);
        let sub = &cmd.children[1];
        assert_eq!(sub.kind, NodeKind::CommandSubstitution);
        assert!(sub.complete);
        // Inner cmd_sub children should now be a single Command with two args.
        assert_eq!(sub.children.len(), 1);
        let inner = &sub.children[0];
        assert_eq!(inner.kind, NodeKind::Command);
        assert_eq!(inner.children.len(), 2);
        assert_eq!(inner.children[0].text, "ls");
        assert_eq!(inner.children[1].text, "foo");
    }

    #[test]
    fn parse_backtick_command_substitution_recurses() {
        let p = parse("echo `pwd`");
        let cmd = &p.children[0];
        let sub = &cmd.children[1];
        assert_eq!(sub.kind, NodeKind::CommandSubstitution);
        assert!(sub.complete);
        assert_eq!(sub.children.len(), 1);
        assert_eq!(sub.children[0].kind, NodeKind::Command);
    }

    #[test]
    fn parse_assignment_with_command_in_program() {
        let p = parse("FOO=bar cmd arg");
        assert_eq!(p.children.len(), 1);
        let stmt = &p.children[0];
        assert_eq!(stmt.kind, NodeKind::AssignmentList);
        assert_eq!(stmt.children.len(), 2);
        assert_eq!(stmt.children[0].kind, NodeKind::Assignment);
        assert_eq!(stmt.children[1].kind, NodeKind::Command);
    }

    #[test]
    fn parse_string_with_command_substitution_inside() {
        // "$(date)" — String contains CommandSubstitution child.
        let p = parse("echo \"$(date)\"");
        let cmd = &p.children[0];
        let arg = &cmd.children[1];
        assert_eq!(arg.kind, NodeKind::String);
        assert!(arg.complete);
        assert_eq!(arg.children.len(), 1);
        assert_eq!(arg.children[0].kind, NodeKind::CommandSubstitution);
    }

    #[test]
    fn parse_unterminated_string_leaves_string_child_incomplete() {
        // Completeness lives on the leaf node — a Command whose String
        // arg never closed is still "structurally a command", so only
        // the String reports incomplete. (Consumers walk children if
        // they need a deep "anything unterminated" check.)
        let p = parse("echo \"hi");
        let cmd = &p.children[0];
        assert_eq!(cmd.kind, NodeKind::Command);
        let arg = &cmd.children[1];
        assert_eq!(arg.kind, NodeKind::String);
        assert!(!arg.complete);
    }
}
