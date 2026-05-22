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
    println!(
        "build-specs: {} loaded, {} skipped",
        summary.loaded, summary.skipped
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
        match process_one(&path, &cli.output, &stem) {
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

fn process_one(input: &Path, output_dir: &Path, stem: &str) -> Result<()> {
    let spec: Spec =
        load_spec_file(input).with_context(|| format!("loading {}", input.display()))?;
    let out_path = output_dir.join(format!("{stem}.json"));
    write_spec_file(&spec, &out_path).with_context(|| format!("writing {}", out_path.display()))?;
    Ok(())
}
