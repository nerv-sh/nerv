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

/// Parse a shell command-line string into a `Program`-rooted node tree.
///
/// The implementation lands in M0-4 chunks 3b–3f. The current body
/// returns an empty `Program` so downstream callers and tests can
/// integrate against the public API.
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
}
