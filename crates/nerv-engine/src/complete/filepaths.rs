use super::{FOLDER_ICON, ci_starts_with};
use crate::ipc::{Suggestion, SuggestionKind};

// ---------------------------------------------------------------------------
// Well-known generator: filepaths / folders (cd, cat, ls, …)
// ---------------------------------------------------------------------------

/// List directory entries matching the trailing-basename portion of
/// `prefix`. Honours `folders_only` (e.g. `cd` uses showFolders=only).
/// `(insertion, display, description, icon)` row tuple emitted by
/// the filesystem walker. Insertion preserves the user's typed
/// directory prefix so `cd ./fo<Tab>` → `cd ./encl/`, not `encl/`.
pub(super) type FilepathRow = (String, String, Option<String>, Option<String>);

/// Wrap `filepaths_at` output as ranked `Suggestion`s. The walker already
/// orders rows exact-case first, then alphabetical; encode that position
/// into `priority` (like the Template arm) so the global
/// `sort_by_priority_then_alpha` preserves it instead of re-alphabetizing
/// — otherwise uppercase `D` (0x44) would float above lowercase `d`,
/// defeating the exact-case ranking. Shared by every filepath call site
/// (template hint, `Generator::Filepaths`, and the smart fallback).
pub(super) fn filepath_suggestions(
    cwd: Option<&std::path::Path>,
    prefix: &str,
    folders_only: bool,
) -> Vec<Suggestion> {
    filepaths_at(cwd, prefix, folders_only)
        .into_iter()
        .flatten()
        .enumerate()
        .map(
            |(idx, (insertion, display, description, icon))| Suggestion {
                insertion,
                display,
                description,
                kind: SuggestionKind::Argument,
                priority: Some(1_000u32.saturating_sub(idx as u32)),
                icon,
                source_ranked: false,
                replace: None,
            },
        )
        .collect()
}

pub(super) fn filepaths_at(
    cwd: Option<&std::path::Path>,
    prefix: &str,
    folders_only: bool,
) -> Option<Vec<FilepathRow>> {
    // Split prefix into (dir_part_preserve_trailing_slash, basename_filter).
    let (dir_part, filter) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let resolved = resolve_filepaths_root(cwd, dir_part)?;
    let entries = std::fs::read_dir(&resolved).ok()?;
    // Each row with, for a plain directory, the path its summary is read
    // from once the order is known.
    let mut out: Vec<(FilepathRow, Option<std::path::PathBuf>)> = Vec::new();
    for e in entries.flatten() {
        let name_os = e.file_name();
        let Some(name) = name_os.to_str() else {
            continue;
        };
        if !ci_starts_with(name, filter) {
            continue;
        }
        // Skip dotfiles unless user explicitly typed a leading dot.
        if name.starts_with('.') && !filter.starts_with('.') {
            continue;
        }
        let ft = e.file_type().ok();
        let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        if folders_only && !is_dir {
            continue;
        }
        let trailing = if is_dir { "/" } else { "" };
        let insertion = format!("{dir_part}{name}{trailing}");
        let display = format!("{name}{trailing}");
        let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
        let summarized = is_dir && !is_symlink;
        let desc = if summarized {
            Some("dir".to_string())
        } else {
            filepaths_desc(&e, is_symlink)
        };
        // Per-row icon: 🔗 for symlinks (checked first; a symlink
        // pointing at a dir still gets the link glyph), 📁 for
        // dirs, 📄 for regular files. All 4-byte UTF-8, pass
        // sanitize_icon's ≤4 byte gate.
        let icon = if is_symlink {
            Some("🔗".to_string())
        } else if is_dir {
            Some(FOLDER_ICON.to_string())
        } else {
            Some("📄".to_string())
        };
        out.push((
            (insertion, display, desc, icon),
            summarized.then(|| e.path()),
        ));
    }
    // Offer `.` and `..` once the user has typed a leading dot —
    // `open .`, `idea .`, `cd ..`. read_dir never yields these two, and
    // a bare `.`/`..` insertion would be dropped by the no-op filter
    // (insertion == the typed token), so emit them with the directory
    // trailing slash like any other dir (`./`, `../`): the slash keeps
    // the insertion distinct from the token and lets `../foo` chain.
    if filter.starts_with('.') {
        for (name, desc) in [(".", "current directory"), ("..", "parent directory")] {
            if !ci_starts_with(name, filter) {
                continue;
            }
            out.push((
                (
                    format!("{dir_part}{name}/"),
                    format!("{name}/"),
                    Some(desc.to_string()),
                    Some(FOLDER_ICON.to_string()),
                ),
                None,
            ));
        }
    }
    // Case-insensitive matching (macOS's default filesystem is
    // case-insensitive), but rank exact-case prefix hits first — `DE`
    // floats `DEEP_LINKING.md` above `deep-linking/`, and `de` the
    // reverse — then alphabetical within each group.
    out.sort_by(|(a, _), (b, _)| {
        let a_exact = a.1.starts_with(filter);
        let b_exact = b.1.starts_with(filter);
        b_exact.cmp(&a_exact).then_with(|| a.1.cmp(&b.1))
    });
    let mut summarized = 0;
    Some(
        out.into_iter()
            .map(|(mut row, dir)| {
                if let Some(dir) = dir.filter(|_| summarized < DIR_SUMMARY_ROWS) {
                    summarized += 1;
                    row.2 = Some(dir_summary(&dir));
                }
                row
            })
            .collect(),
    )
}

/// How many leading directories of a listing get an item count
/// ([`dir_summary`], one `read_dir` each); the rest say `dir`. Directories
/// are counted, not rows: the daemon lifts folders above files, so the
/// ones that lead the popup are the first directories, wherever files
/// sort. Twice the popup's window of 8: the first page down still shows
/// counts, and a folder with hundreds of subfolders costs the same as one
/// with sixteen.
pub(super) const DIR_SUMMARY_ROWS: usize = 16;
/// The description of a row that is not a plain directory — those are
/// summarized by [`filepaths_at`] once it knows which rows lead.
fn filepaths_desc(entry: &std::fs::DirEntry, is_symlink: bool) -> Option<String> {
    if is_symlink {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            return Some(format!("→ {}", target.display()));
        }
        return Some("symlink".into());
    }
    let meta = entry.metadata().ok()?;
    Some(human_size(meta.len()))
}

/// One-line summary for a directory entry. Reads the dir to count
/// visible children (excluding dotfiles). Cap the iteration at 200
/// entries to keep latency bounded for huge dirs (node_modules).
pub(super) fn dir_summary(path: &std::path::Path) -> String {
    let Ok(rd) = std::fs::read_dir(path) else {
        return "dir".into();
    };
    let mut count: u32 = 0;
    let mut truncated = false;
    for (i, e) in rd.flatten().enumerate() {
        if i >= 200 {
            truncated = true;
            break;
        }
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with('.') {
            continue;
        }
        count += 1;
    }
    if count == 0 {
        "empty".into()
    } else if truncated {
        format!("{count}+ items")
    } else if count == 1 {
        "1 item".into()
    } else {
        format!("{count} items")
    }
}

pub(super) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}{}", UNITS[0])
    } else if v >= 100.0 {
        format!("{v:.0}{}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.2}{}", UNITS[u])
    }
}

/// Resolve a (possibly relative, possibly tilde-prefixed) directory
/// string against the client's cwd. Returns the canonical filesystem
/// path to read, or `None` if neither cwd nor `HOME` is known.
fn resolve_filepaths_root(
    cwd: Option<&std::path::Path>,
    dir_part: &str,
) -> Option<std::path::PathBuf> {
    if dir_part.is_empty() {
        return cwd
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok());
    }
    if dir_part.starts_with('/') {
        return Some(std::path::PathBuf::from(dir_part));
    }
    if let Some(rest) = dir_part.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(std::path::PathBuf::from(home).join(rest));
    }
    if dir_part == "~" {
        let home = std::env::var("HOME").ok()?;
        return Some(std::path::PathBuf::from(home));
    }
    let base = cwd
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())?;
    Some(base.join(dir_part))
}
