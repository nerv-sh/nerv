// ---------------------------------------------------------------------------
// Well-known generator: SSH host enumeration (ssh, scp, sftp, mosh, rsync)
// ---------------------------------------------------------------------------

/// Collect SSH hosts from `~/.ssh/known_hosts` + `~/.ssh/config` (with
/// `Include` directive support, max one level deep). Dedup, sort, no
/// score. Returns `None` only when `HOME` is unset; an empty Vec
/// otherwise (caller surfaces that as "no completions").
pub(super) fn ssh_hosts() -> Option<Vec<String>> {
    let home = std::env::var_os("HOME")?;
    let ssh_dir = std::path::PathBuf::from(home).join(".ssh");
    let mut hosts: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    ssh_hosts_from_known(&ssh_dir.join("known_hosts"), &mut hosts);
    ssh_hosts_from_config(&ssh_dir.join("config"), &ssh_dir, &mut hosts, 0);
    Some(hosts.into_iter().collect())
}
fn ssh_hosts_from_known(path: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        // `known_hosts` lines: `<hosts> <key-type> <key>` where `<hosts>`
        // is a comma-list of `host` / `host,host:port` / `[host]:port`
        // / `|1|salt|hash` (hashed — skip). Skip comments + empty.
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("|1|") {
            continue;
        }
        let Some(host_field) = trimmed.split_whitespace().next() else {
            continue;
        };
        for entry in host_field.split(',') {
            let cleaned = entry
                .trim_start_matches('[')
                .split([']', ':'])
                .next()
                .unwrap_or(entry)
                .trim();
            if !cleaned.is_empty() && !cleaned.contains('*') {
                out.insert(cleaned.to_string());
            }
        }
    }
}

fn ssh_hosts_from_config(
    path: &std::path::Path,
    ssh_dir: &std::path::Path,
    out: &mut std::collections::BTreeSet<String>,
    depth: usize,
) {
    if depth > 1 {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let rest = parts.next().unwrap_or("").trim();
        if key == "host" {
            for entry in rest.split_whitespace() {
                if !entry.contains('*') && !entry.contains('?') {
                    out.insert(entry.to_string());
                }
            }
        } else if key == "include" {
            for inc in rest.split_whitespace() {
                let p = if inc.starts_with('/') {
                    std::path::PathBuf::from(inc)
                } else {
                    ssh_dir.join(inc)
                };
                ssh_hosts_from_config(&p, ssh_dir, out, depth + 1);
            }
        }
    }
}
