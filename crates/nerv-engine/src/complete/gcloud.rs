//! `gcloud_config` — values from the local gcloud configuration files.
//!
//! gcloud's global flags (`--project`, `--configuration`, `--region`,
//! `--zone`) are typed before any subcommand, and the vendored spec
//! declares every one of their args empty. Asking gcloud itself costs
//! ~1s of Python startup — past the engine's 800ms generator cap — so
//! the engine reads the local INI files instead:
//!
//! ```text
//! $CLOUDSDK_CONFIG/configurations/config_<name>
//! $CLOUDSDK_CONFIG/active_config
//! ```
//!
//! `CLOUDSDK_CONFIG` is gcloud's own override; the default is
//! `~/.config/gcloud`. Strictly local: no network, no subprocess. A
//! missing directory, an unreadable file, or an unset key degrades to
//! "no rows" — completion must never fail because configuration is thin.

use std::collections::BTreeSet;
use std::path::Path;

/// Values for `field` across the local configurations, or `None` when
/// there is no configuration directory at all.
pub(super) fn gcloud_config_values(field: &str) -> Option<Vec<String>> {
    let dir = std::env::var_os("CLOUDSDK_CONFIG")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config/gcloud"))
        })?;
    values_in(&dir, field)
}

/// Directory-injected form, so tests point it at a tempdir instead of
/// mutating `HOME`/`CLOUDSDK_CONFIG` for the whole test process.
fn values_in(dir: &Path, field: &str) -> Option<Vec<String>> {
    let confs = dir.join("configurations");
    let mut names: Vec<String> = std::fs::read_dir(&confs)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("config_")
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect();
    if names.is_empty() {
        return Some(Vec::new());
    }
    names.sort();
    // The active configuration is the one gcloud's own commands use by
    // default, so it leads; the rest stay in file-name order.
    if let Ok(active) = std::fs::read_to_string(dir.join("active_config")) {
        let active = active.trim();
        if !active.is_empty() {
            names.retain(|n| n != active);
            names.insert(0, active.to_string());
        }
    }
    let mut seen = BTreeSet::new();
    let values = names
        .into_iter()
        .filter_map(|name| {
            // `--configuration` names the configuration itself; the other
            // fields live inside the file.
            let value = if field == "configuration" {
                name.clone()
            } else {
                read_ini_value(&confs.join(format!("config_{name}")), field)?
            };
            let value = value.trim().to_string();
            (!value.is_empty() && seen.insert(value.clone())).then_some(value)
        })
        .collect();
    Some(values)
}

/// One key from a gcloud configuration file (`[core] project = …`,
/// `[compute] region = …`). Malformed lines and unknown fields are
/// skipped — the caller moves on to the next configuration.
fn read_ini_value(path: &Path, field: &str) -> Option<String> {
    let section = match field {
        "project" => "core",
        "region" | "zone" => "compute",
        _ => return None,
    };
    let text = std::fs::read_to_string(path).ok()?;
    let mut current = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if current == section && key.trim() == field {
            return Some(value.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nerv-gcloud-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("configurations")).unwrap();
        dir
    }

    fn write_config(dir: &Path, name: &str, body: &str) {
        std::fs::write(
            dir.join("configurations").join(format!("config_{name}")),
            body,
        )
        .unwrap();
    }

    #[test]
    fn reads_each_field_from_the_local_configurations() {
        let dir = temp_dir("fields");
        write_config(
            &dir,
            "default",
            "[core]\nproject = alpha\naccount = a@example.com\n\
             [compute]\nregion = us-central1\nzone = us-central1-a\n",
        );
        write_config(
            &dir,
            "work",
            "[core]\nproject = beta\n[compute]\nzone = europe-west1-b\n",
        );

        assert_eq!(
            values_in(&dir, "configuration"),
            Some(vec!["default".to_string(), "work".to_string()])
        );
        assert_eq!(
            values_in(&dir, "project"),
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
        // Only the configuration that sets it contributes.
        assert_eq!(
            values_in(&dir, "region"),
            Some(vec!["us-central1".to_string()])
        );
        assert_eq!(
            values_in(&dir, "zone"),
            Some(vec![
                "us-central1-a".to_string(),
                "europe-west1-b".to_string()
            ])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_active_configuration_leads_and_duplicates_collapse() {
        let dir = temp_dir("active");
        write_config(&dir, "default", "[core]\nproject = shared\n");
        write_config(&dir, "work", "[core]\nproject = shared\n");
        std::fs::write(dir.join("active_config"), "work\n").unwrap();

        assert_eq!(
            values_in(&dir, "configuration"),
            Some(vec!["work".to_string(), "default".to_string()])
        );
        assert_eq!(values_in(&dir, "project"), Some(vec!["shared".to_string()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_yields_no_rows() {
        let missing = std::env::temp_dir().join("nerv-gcloud-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(values_in(&missing, "project"), None);
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let dir = temp_dir("malformed");
        write_config(&dir, "broken", "not an ini line\n[core\nproject = alpha\n");
        write_config(&dir, "ok", "[core]\nproject = beta\n");
        assert_eq!(
            values_in(&dir, "project"),
            Some(vec!["beta".to_string()]),
            "the broken file contributes nothing; the good one still answers"
        );
        assert_eq!(values_in(&dir, "bogus_field"), Some(vec![]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
