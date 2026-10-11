// ---------------------------------------------------------------------------
// Well-known generator: shell history words
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// `template: history` — last N unique words from $HISTFILE.
// ---------------------------------------------------------------------------

const HISTORY_MAX_RETURN: usize = 200;
const HISTORY_TAIL_LINES: usize = 2000;

/// Pull the last ~2000 lines from the user's shell history file and
/// return the unique whitespace tokens contained, last-seen first.
/// Looks at `$HISTFILE`, then `~/.zsh_history`, then `~/.bash_history`.
pub(super) fn shell_history_entries() -> Option<Vec<String>> {
    let path = history_file()?;
    // zsh stores history with `\xNN`-escaped bytes that aren't valid
    // UTF-8 byte sequences (e.g. Korean chars under `setopt
    // EXTENDED_HISTORY`). Read raw bytes and lossy-decode — losing
    // bad sequences as `U+FFFD` is fine; we only emit ASCII tokens.
    let bytes = std::fs::read(&path).ok()?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    // Iterate newest-first.
    let lines: Vec<&str> = raw.lines().rev().take(HISTORY_TAIL_LINES).collect();
    for line in lines {
        // zsh extended-history lines look like `: 1700000000:0;cmd args`.
        // Trim the metadata prefix when present.
        let payload = match line.find(';') {
            Some(i) if line.starts_with(": ") => &line[i + 1..],
            _ => line,
        };
        for token in payload.split_whitespace() {
            if seen.insert(token.to_string()) {
                out.push(token.to_string());
                if out.len() >= HISTORY_MAX_RETURN {
                    return Some(out);
                }
            }
        }
    }
    Some(out)
}

fn history_file() -> Option<std::path::PathBuf> {
    if let Ok(v) = std::env::var("HISTFILE") {
        if !v.is_empty() {
            let p = std::path::PathBuf::from(v);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let home = std::env::var_os("HOME")?;
    let home = std::path::PathBuf::from(home);
    for name in &[".zsh_history", ".bash_history"] {
        let p = home.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
