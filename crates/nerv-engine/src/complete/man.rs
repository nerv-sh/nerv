// ---------------------------------------------------------------------------
// Well-known generator: man page enumeration (`man`)
// ---------------------------------------------------------------------------

/// Walk the user's `MANPATH` (or the common defaults) for `manN/*`
/// entries and return the bare page name (no section suffix, no
/// `.gz`). Matches the surface area of Fig's `generateManualPages`
/// closure without spawning `man -k`.
pub(super) fn man_pages() -> Option<Vec<String>> {
    let roots = man_path_roots();
    if roots.is_empty() {
        return None;
    }
    let mut pages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for root in roots {
        let Ok(sections) = std::fs::read_dir(&root) else {
            continue;
        };
        for sec in sections.flatten() {
            let name = sec.file_name();
            let Some(s) = name.to_str() else {
                continue;
            };
            if !s.starts_with("man") || s.len() < 4 {
                continue;
            }
            let Ok(files) = std::fs::read_dir(sec.path()) else {
                continue;
            };
            for f in files.flatten() {
                let fname = f.file_name();
                let Some(fs) = fname.to_str() else { continue };
                if let Some(page) = man_page_stem(fs) {
                    pages.insert(page);
                }
            }
        }
    }
    Some(pages.into_iter().collect())
}

fn man_page_stem(file_name: &str) -> Option<String> {
    let trimmed = file_name.strip_suffix(".gz").unwrap_or(file_name);
    // `git.1` / `printf.3` / `man.1posix` — strip the last dot and
    // anything after.
    let dot = trimmed.rfind('.')?;
    let stem = &trimmed[..dot];
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn man_path_roots() -> Vec<std::path::PathBuf> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("MANPATH") {
        for p in v.split(':') {
            if !p.is_empty() {
                roots.push(std::path::PathBuf::from(p));
            }
        }
    }
    for fallback in [
        "/usr/share/man",
        "/usr/local/share/man",
        "/opt/homebrew/share/man",
        "/Library/Developer/CommandLineTools/usr/share/man",
    ] {
        let p = std::path::PathBuf::from(fallback);
        if p.exists() && !roots.contains(&p) {
            roots.push(p);
        }
    }
    roots
}
