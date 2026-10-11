// ---------------------------------------------------------------------------
// Well-known generator: Makefile target enumeration (`make`)
// ---------------------------------------------------------------------------

/// Parse `Makefile` / `makefile` / `GNUmakefile` in the current
/// working directory and return target names. Recognises lines of
/// the form `<name>:` where `<name>` is composed of identifier-safe
/// characters — same surface area as Fig's `listTargets` closure but
/// without booting Node.
pub(super) fn makefile_targets(cwd: Option<&std::path::Path>) -> Option<Vec<String>> {
    let dir = cwd
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())?;
    let path = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut targets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in raw.lines() {
        // Skip recipe lines (start with a tab) and continuation chunks.
        if line.starts_with('\t') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let head = &line[..colon];
        // Skip `target = value` style variables — `:=` rules out
        // direct-set, and a leading `#` is a comment.
        if head.contains('=') || head.trim_start().starts_with('#') {
            continue;
        }
        for raw_target in head.split_whitespace() {
            if is_makefile_target_name(raw_target) && !raw_target.starts_with('.') {
                targets.insert(raw_target.to_string());
            }
        }
    }
    Some(targets.into_iter().collect())
}

fn is_makefile_target_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
}
