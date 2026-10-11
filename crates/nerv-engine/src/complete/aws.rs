// ---------------------------------------------------------------------------
// Well-known generator: aws profile names
// ---------------------------------------------------------------------------

/// Profile names as `aws configure list-profiles` reports them, read straight
/// from `~/.aws/{config,credentials}`. Recovers the CLI generator without the
/// ~1s cold-start spawn (see [`execute_template_generator`]).
pub(super) fn aws_profiles() -> Option<Vec<String>> {
    let home = std::env::var_os("HOME")?;
    let aws_dir = std::path::PathBuf::from(home).join(".aws");
    let config = std::fs::read_to_string(aws_dir.join("config")).unwrap_or_default();
    let credentials = std::fs::read_to_string(aws_dir.join("credentials")).unwrap_or_default();
    let out = parse_aws_profiles(&config, &credentials);
    if out.is_empty() { None } else { Some(out) }
}

/// The `[…]` section name of an ini line, trimmed, or `None`.
fn ini_section(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
}

/// Merge profile names from the two AWS shared files, first-seen order.
/// `config` uses `[default]` + `[profile NAME]` (other section kinds —
/// `[sso-session …]`, `[services …]` — are not profiles); `credentials` uses
/// bare `[NAME]`.
pub(super) fn parse_aws_profiles(config: &str, credentials: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    // config `[default]` / `[profile NAME]`; then credentials `[NAME]`.
    let config_names = config.lines().filter_map(|line| match ini_section(line)? {
        "default" => Some("default"),
        section => section.strip_prefix("profile ").map(str::trim),
    });
    let cred_names = credentials.lines().filter_map(ini_section);
    for name in config_names.chain(cred_names) {
        if !name.is_empty() && seen.insert(name.to_string()) {
            out.push(name.to_string());
        }
    }
    out
}
