//! `nerv-spec-build` — transpile `vendor/withfig-autocomplete/src/*.ts` into
//! the static JSON format documented in `docs/spec-conversion-policy.md`.
//!
//! Pipeline (M0-2 / M1):
//!   1. Read each `<name>.ts` from the input directory.
//!   2. Parse to TypeScript AST via swc_core.
//!   3. Classify into Tier A / B / C (see `Tier` in nerv_engine::spec).
//!   4. Tier A/B → emit `<name>.json` and a manifest entry.
//!   5. Tier C → log only.
//!
//! Build is **deterministic**: same input → same output, no timestamps
//! inside per-spec files (only in `manifest.json`).

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "nerv-spec-build",
    about = "Transpile withfig/autocomplete TS specs into static JSON.",
)]
struct Args {
    /// Path to `vendor/withfig-autocomplete/src/`.
    #[arg(long)]
    input: PathBuf,
    /// Path to write the transpiled `*.json` files and `manifest.json`.
    #[arg(long)]
    output: PathBuf,
    /// Optional whitelist (filenames without `.ts` suffix). When empty,
    /// process every spec found in `input`.
    #[arg(long)]
    only: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let args = Args::parse();
    info!(input = %args.input.display(), output = %args.output.display(), "spec-build (M0-2 stub)");

    if !args.input.is_dir() {
        anyhow::bail!("input is not a directory: {}", args.input.display());
    }
    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("cannot create output dir: {}", args.output.display()))?;

    let mut considered = 0usize;
    for entry in std::fs::read_dir(&args.input)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ts") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !args.only.is_empty() && !args.only.iter().any(|w| w == stem) {
            continue;
        }
        considered += 1;
        warn!(spec = stem, "transpile not yet implemented (M0-2)");
    }

    info!(specs = considered, "scanned (no output yet — M0-2 fills this in)");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("NERV_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();
}
