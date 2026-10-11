use super::{cached_template_generator, fuzzy_subsequence_match};
use crate::config::MatchMode;

// ---------------------------------------------------------------------------
// Well-known generator: zoxide directory history (z, zoxide)
// ---------------------------------------------------------------------------

/// Shell-quote a path for insertion when it holds characters the shell
/// would split or interpret. A plain path (ASCII alnum + a small safe
/// set) inserts raw; anything else is single-quoted with embedded single
/// quotes escaped, so `z /My Docs/x` becomes `z '/My Docs/x'`.
pub(super) fn shell_quote_arg(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-~+=:@,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Rank zoxide rows against the typed query. `rows` arrive score-desc
/// (frecency). Groups, in order of intent: folder-name prefix hits, then
/// folder-name substring hits, then path-only hits (the name doesn't
/// match but a parent directory does). Frecency order is preserved
/// within each group, and non-matching rows drop out. This is what makes
/// `z enc` surface `encl` (a name-prefix hit) above `app`/`apps`, whose
/// only match is the shared `.../encl/...` parent path.
pub(super) fn rank_zoxide_matches(
    rows: Vec<(String, String, f64)>,
    query: &str,
    mode: MatchMode,
) -> Vec<(String, String, f64)> {
    let needle = query.to_lowercase();
    let (mut name_prefix, mut name_substr, mut path_only) = (Vec::new(), Vec::new(), Vec::new());
    for row in rows {
        let name_lc = row.0.to_lowercase();
        // Name bucket 2: substring always, plus subsequence when the
        // user opted into fuzzy — so `z mz` finds `muzly` by abbreviation
        // while name hits still rank ahead of path-only ones.
        let name_secondary = name_lc.contains(&needle)
            || (mode == MatchMode::Fuzzy && fuzzy_subsequence_match(&name_lc, &needle));
        if needle.is_empty() || name_lc.starts_with(&needle) {
            name_prefix.push(row);
        } else if name_secondary {
            name_substr.push(row);
        } else if row.1.to_lowercase().contains(&needle) {
            path_only.push(row);
        }
    }
    name_prefix
        .into_iter()
        .chain(name_substr)
        .chain(path_only)
        .collect()
}

/// Resolve the zoxide / zsh-z directory history. Tries `zoxide query
/// --list --score` first (200ms cap, cached); on failure falls back
/// to the zsh-z `~/.z` flat file (or `$_Z_DATA` / `$ZSHZ_DATA` env
/// override). Returns `(folder_name, full_path, score)` tuples
/// sorted by descending score. The folder_name is the last path
/// segment — what the user usually wants to insert.
pub(super) fn zoxide_query() -> Option<Vec<(String, String, f64)>> {
    if let Some(rows) = zoxide_via_command() {
        if !rows.is_empty() {
            return Some(sorted_by_score(rows));
        }
    }
    zoxide_via_z_file().map(sorted_by_score)
}

fn zoxide_via_command() -> Option<Vec<(String, String, f64)>> {
    let key = vec![
        "zoxide".to_string(),
        "query".to_string(),
        "--list".to_string(),
        "--score".to_string(),
    ];
    // zoxide keeps a single global db — cwd-independent.
    let lines = cached_template_generator(&key, None)?;
    let mut rows: Vec<(String, String, f64)> = Vec::new();
    for line in lines {
        // Each line: "<spaces><score> <path>".
        let trimmed = line.trim_start();
        let mut split = trimmed.splitn(2, char::is_whitespace);
        let score_str = split.next()?;
        let path = split.next()?.trim().to_string();
        let score: f64 = score_str.parse().ok()?;
        rows.push((folder_name(&path), path, score));
    }
    Some(rows)
}

pub(super) fn zoxide_via_z_file() -> Option<Vec<(String, String, f64)>> {
    let path = z_history_file()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut rows: Vec<(String, String, f64)> = Vec::new();
    for line in raw.lines() {
        // zsh-z / z.sh format: "<path>|<score>|<unixtime>".
        let mut parts = line.splitn(3, '|');
        let p = parts.next()?.trim().to_string();
        let s = parts.next()?.trim();
        let score: f64 = s.parse().ok()?;
        rows.push((folder_name(&p), p, score));
    }
    Some(rows)
}

pub(super) fn z_history_file() -> Option<std::path::PathBuf> {
    if let Ok(v) = std::env::var("ZSHZ_DATA") {
        if !v.is_empty() {
            return Some(std::path::PathBuf::from(v));
        }
    }
    if let Ok(v) = std::env::var("_Z_DATA") {
        if !v.is_empty() {
            return Some(std::path::PathBuf::from(v));
        }
    }
    let home = std::env::var("HOME").ok()?;
    let candidate = std::path::PathBuf::from(home).join(".z");
    candidate.exists().then_some(candidate)
}

fn folder_name(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn sorted_by_score(mut rows: Vec<(String, String, f64)>) -> Vec<(String, String, f64)> {
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    rows
}
