//! `examples/specs/claude.json` is the living example of a user overlay spec
//! (`docs/spec-conversion-policy.md` §6.1): the file a user copies into
//! `~/.config/nerv/specs/`. This test keeps it parseable and useful — layered
//! over the hand-rolled fixture pack it must load, complete, and be
//! attributed to the overlay layer, while the bundled fixtures keep serving.

use nerv_engine::complete::workspace_fixture_specs_dir;
use nerv_engine::{SpecRegistry, complete};
use std::path::PathBuf;

fn examples_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", "examples", "specs"]
        .iter()
        .collect()
}

#[test]
fn claude_example_loads_as_overlay_and_completes() {
    let reg = SpecRegistry::at_dirs(&[examples_dir(), workspace_fixture_specs_dir()]);

    let claude = reg
        .lookup("claude")
        .expect("examples/specs/claude.json must parse");
    assert!(
        claude.subcommands.len() >= 12,
        "sample should cover the top-level claude commands, got {}",
        claude.subcommands.len()
    );
    assert!(
        claude.options.len() >= 15,
        "sample should cover the common root flags, got {}",
        claude.options.len()
    );

    // The bundled layer underneath is untouched by the overlay.
    assert!(reg.lookup("git").is_some(), "fixture git must still serve");

    let names = |line: &str| -> Vec<String> {
        complete(line, line.len(), &reg)
            .items
            .iter()
            .map(|s| s.insertion.clone())
            .collect()
    };
    let subs = names("claude ");
    assert!(subs.len() >= 10, "`claude ` → {subs:?}");
    assert!(subs.iter().any(|s| s == "mcp"), "`claude ` → {subs:?}");
    let flags = names("claude --");
    assert!(flags.len() >= 10, "`claude --` → {flags:?}");
    assert!(
        flags.iter().any(|s| s == "--model"),
        "`claude --` → {flags:?}"
    );
    let mcp = names("claude mcp ");
    assert!(mcp.iter().any(|s| s == "add"), "`claude mcp ` → {mcp:?}");

    // spec list / doctor attribute the stem to the overlay layer.
    assert_eq!(reg.stems_served_from(&examples_dir()), ["claude"]);
}
