//! build-specs — bulk-validate / copy spec JSON files at build time.
//!
//! Walks `--input <dir>` for `*.json`, parses each as a
//! [`nerv_engine::Spec`], and writes the canonicalized form to
//! `--output <dir>/<stem>.json`. `--only <name>` filters by file
//! stem (repeatable).
//!
//! The TS → JSON conversion itself is **out of scope** for M0;
//! upstream `vendor/withfig-autocomplete/src/*.ts` is converted by
//! a separate node-side step that lands later. This binary is the
//! Rust-side validator + canonicalizer.
//!
//! Refs: PRD v0.6 §10 M0-6, CLAUDE.md §5.

use anyhow::{Context, Result, bail};
use clap::Parser;
use nerv_engine::{Spec, load_spec_file, write_spec_file};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "build-specs",
    about = "Validate + canonicalize Nerv spec JSON files"
)]
struct Cli {
    /// Directory containing input *.json spec files.
    #[arg(long)]
    input: PathBuf,
    /// Directory to write canonicalized *.json (created if missing).
    #[arg(long)]
    output: PathBuf,
    /// Restrict to specs whose file stem matches one of these names.
    /// Repeatable. If empty, all *.json files are processed.
    #[arg(long, value_name = "NAME")]
    only: Vec<String>,
    /// Gzip-compress the output: write `<stem>.json.gz` instead of
    /// `<stem>.json`. SpecRegistry lookup auto-detects either form.
    #[arg(long)]
    compress: bool,
    /// Print one line per spec, including skipped + errored.
    #[arg(long)]
    verbose: bool,
}

#[derive(Debug, Default)]
struct Summary {
    loaded: usize,
    skipped: usize,
    failed: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.input.is_dir() {
        bail!("--input is not a directory: {}", cli.input.display());
    }
    let summary = process_directory(&cli)?;
    if summary.failed > 0 {
        eprintln!(
            "build-specs: {} loaded, {} skipped, {} FAILED",
            summary.loaded, summary.skipped, summary.failed
        );
        std::process::exit(1);
    }
    // Stamp the cache with the schema version so the daemon's E5 gate can
    // reject a swapped-in cache from a different nerv build.
    nerv_engine::manifest::write_manifest(&cli.output, summary.loaded)
        .with_context(|| format!("writing manifest to {}", cli.output.display()))?;
    println!(
        "build-specs: {} loaded, {} skipped (manifest v{})",
        summary.loaded,
        summary.skipped,
        nerv_engine::manifest::SUPPORTED_SCHEMA_VERSION
    );
    Ok(())
}

fn process_directory(cli: &Cli) -> Result<Summary> {
    let mut summary = Summary::default();
    let entries =
        fs::read_dir(&cli.input).with_context(|| format!("reading {}", cli.input.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !cli.only.is_empty() && !cli.only.iter().any(|n| n == &stem) {
            if cli.verbose {
                println!("skip {stem}");
            }
            summary.skipped += 1;
            continue;
        }
        match process_one(&path, &cli.output, &stem, cli.compress) {
            Ok(()) => {
                summary.loaded += 1;
                if cli.verbose {
                    println!("load {stem}");
                }
            }
            Err(e) => {
                summary.failed += 1;
                eprintln!("FAIL {stem}: {e:#}");
            }
        }
    }
    Ok(summary)
}

fn process_one(input: &Path, output_dir: &Path, stem: &str, compress: bool) -> Result<()> {
    let spec: Spec =
        load_spec_file(input).with_context(|| format!("loading {}", input.display()))?;
    let ext = if compress { "json.gz" } else { "json" };
    let out_path = output_dir.join(format!("{stem}.{ext}"));

    // A spec too big to parse on one keystroke gives up its large
    // top-level subcommands to `<stem>/<name>.<ext>`; the root keeps a
    // stub for each. `aws` is 117 MB and 1.3 s of parsing, of which a
    // line like `aws s3 ls` needs 90 KB (spec-conversion-policy §6.3).
    let (spec, extracted) = nerv_engine::split_oversized(spec);
    let subtree_dir = output_dir.join(stem);
    if extracted.is_empty() {
        // A spec that stopped being oversized (or was never split)
        // must not keep serving a stale subtree dir from an earlier
        // build: the root no longer points at it, so it is dead weight
        // that `spec list` would still count.
        if subtree_dir.is_dir() {
            std::fs::remove_dir_all(&subtree_dir)
                .with_context(|| format!("removing stale {}", subtree_dir.display()))?;
        }
    } else {
        std::fs::create_dir_all(&subtree_dir)
            .with_context(|| format!("creating {}", subtree_dir.display()))?;
        for (name, subtree) in &extracted {
            let path = subtree_dir.join(format!("{name}.{ext}"));
            write_spec_file(subtree, &path)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    write_spec_file(&spec, &out_path).with_context(|| format!("writing {}", out_path.display()))?;
    // Defensive cleanup: SpecRegistry::load_from_disk prefers a plain
    // `<stem>.json` over the `<stem>.json.gz` form when both exist.
    // A prior install of an older / smaller fixture (e.g. the bundled
    // 4-subcommand `git.json` test fixture) would otherwise shadow
    // the freshly-written gzipped spec and only its subset would be
    // returned by `complete()`. Symmetric: writing plain also clears
    // the .gz so users don't get confused stale data either way.
    let stale = if compress {
        output_dir.join(format!("{stem}.json"))
    } else {
        output_dir.join(format!("{stem}.json.gz"))
    };
    if stale.exists() {
        std::fs::remove_file(&stale)
            .with_context(|| format!("removing stale {}", stale.display()))?;
    }
    Ok(())
}
