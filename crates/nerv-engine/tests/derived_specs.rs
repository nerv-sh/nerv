//! Integration: deriving a spec by actually running a command.
//!
//! Every case points `derive_from_binary` at a script this test wrote,
//! so nothing depends on which tools the machine happens to have and
//! `PATH` is never mutated (it is process-global; mutating it would
//! make these tests order-dependent under the default parallel runner).

use nerv_engine::SpecRegistry;
use nerv_engine::derived::{derive_from_binary, is_derivable};
use std::path::{Path, PathBuf};

/// Write an executable shell script that prints `output` to stdout.
fn fake_bin(dir: &Path, name: &str, output: &str) -> PathBuf {
    write_bin(
        dir,
        name,
        &format!("cat <<'NERV_EOF'\n{output}\nNERV_EOF\n"),
    )
}

fn write_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).expect("write script");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

const HELP: &str = "\
Usage: mytool <command> [options]

Commands:
  build       Build the project
  clean       Remove build artifacts
  publish     Upload a release

Options:
  -v, --verbose        Print more
  -o, --output <path>  Where to write
  -h, --help           Show this help
";

#[test]
fn derives_a_spec_from_the_commands_own_help() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = fake_bin(tmp.path(), "mytool", HELP);
    let out_dir = tmp.path().join("derived");

    let written = derive_from_binary("mytool", &bin, &out_dir).expect("derived");
    assert_eq!(written, out_dir.join("mytool.json"));

    // Read it back the way the registry will.
    let registry = SpecRegistry::at_dir(&out_dir);
    let spec = registry.lookup("mytool").expect("spec loads");
    assert_eq!(spec.name, "mytool");
    let subs: Vec<&str> = spec.subcommands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(subs, ["build", "clean", "publish"]);
    let opts: Vec<&str> = spec
        .options
        .iter()
        .flat_map(|o| o.names.iter().map(|n| n.as_str()))
        .collect();
    assert!(opts.contains(&"--verbose"), "{opts:?}");
    assert!(opts.contains(&"--output"), "{opts:?}");
}

/// Help printed to stderr instead of stdout is still help — several
/// CLIs do it, and one of them is the reason this is tested.
#[test]
fn reads_help_from_stderr_too() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = write_bin(
        tmp.path(),
        "stderrtool",
        &format!("cat >&2 <<'NERV_EOF'\n{HELP}\nNERV_EOF\nexit 1\n"),
    );
    let out_dir = tmp.path().join("derived");
    let written = derive_from_binary("stderrtool", &bin, &out_dir).expect("derived from stderr");
    assert!(written.exists());
}

/// A command that answers `--help` with an error must not become a
/// spec, and must not leave a file behind.
#[test]
fn refuses_to_write_a_spec_for_unhelpful_output() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = write_bin(
        tmp.path(),
        "grumpy",
        "echo 'grumpy: unknown flag' >&2\nexit 2\n",
    );
    let out_dir = tmp.path().join("derived");
    assert!(derive_from_binary("grumpy", &bin, &out_dir).is_none());
    assert!(
        !out_dir.join("grumpy.json").exists(),
        "no file for a non-spec"
    );
}

/// A command that never exits is abandoned, not waited on forever.
#[test]
fn a_hanging_command_times_out() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = write_bin(tmp.path(), "hangs", "sleep 30\n");
    let out_dir = tmp.path().join("derived");
    let start = std::time::Instant::now();
    assert!(derive_from_binary("hangs", &bin, &out_dir).is_none());
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
}

/// The second call reuses the file on disk instead of running the
/// command again — otherwise every keystroke would spawn a process.
#[test]
fn a_current_file_is_reused_without_running_the_command() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let marker = tmp.path().join("ran");
    // Each run appends a line, so the file counts invocations.
    let bin = write_bin(
        tmp.path(),
        "counted",
        &format!(
            "echo run >> {}\ncat <<'NERV_EOF'\n{HELP}\nNERV_EOF\n",
            marker.display()
        ),
    );
    let out_dir = tmp.path().join("derived");

    derive_from_binary("counted", &bin, &out_dir).expect("first derive");
    // Count spawns *after* the first derive rather than pinning its
    // total: a derive may legitimately try more than one help flag, and
    // which ones it needs is not what this test is about.
    let after_first = run_count(&marker);
    assert!(after_first >= 1, "the first derive must ask the command");

    derive_from_binary("counted", &bin, &out_dir).expect("second derive");
    derive_from_binary("counted", &bin, &out_dir).expect("third derive");

    assert_eq!(
        run_count(&marker),
        after_first,
        "a current file must be reused without running the command again"
    );
}

fn run_count(marker: &Path) -> usize {
    std::fs::read_to_string(marker)
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

/// Upgrading the tool invalidates its derived spec: a binary newer than
/// the file means the help may list new commands.
#[test]
fn a_newer_binary_forces_a_re_derive() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let marker = tmp.path().join("ran");
    let script = format!(
        "echo run >> {}\ncat <<'NERV_EOF'\n{HELP}\nNERV_EOF\n",
        marker.display()
    );
    let bin = write_bin(tmp.path(), "upgraded", &script);
    let out_dir = tmp.path().join("derived");
    derive_from_binary("upgraded", &bin, &out_dir).expect("first derive");
    let after_first = run_count(&marker);

    // Rewrite the binary so its mtime moves past the derived file's.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let bin = write_bin(tmp.path(), "upgraded", &script);
    derive_from_binary("upgraded", &bin, &out_dir).expect("re-derive");

    assert!(
        run_count(&marker) > after_first,
        "a newer binary must be asked again (still {after_first} runs)"
    );
}

/// Names that cannot be a plain executable are never run.
#[test]
fn undervable_names_are_rejected_before_anything_runs() {
    for bad in [
        "",
        "x",
        "./foo",
        "/usr/bin/ls",
        "a b",
        "cd",
        "export",
        "eval",
        "source",
        "if",
    ] {
        assert!(!is_derivable(bad), "{bad:?} must not be derivable");
    }
    for good in ["mytool", "aicommit2", "zeph", "gh", "docker-compose", "g++"] {
        assert!(is_derivable(good), "{good:?} should be derivable");
    }
}

/// A tool that predates `--help` still derives, from its man page.
///
/// `ls` is the case that shaped the code: `ls --help` is a two-line
/// error, and `ls -h` prints a *directory listing* — long, multi-line,
/// and nothing like help. Choosing a candidate by how help-shaped its
/// output looks accepts that listing and never reaches man. Choosing by
/// whether it parses picks the man page.
#[test]
fn a_tool_without_help_flags_derives_from_its_man_page() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ls = Path::new("/bin/ls");
    if !ls.exists() {
        return;
    }
    let out_dir = tmp.path().join("derived");
    let derived = derive_from_binary("ls", ls, &out_dir);

    if !Path::new("/usr/bin/man").exists() {
        assert!(derived.is_none(), "no man binary → no derivation");
        return;
    }
    let path = derived.expect("ls derives from man");
    let spec = nerv_engine::spec_loader::load_spec_file(&path).expect("spec loads");
    assert!(
        spec.options.len() >= 20,
        "expected ls's option list, got {}",
        spec.options.len()
    );
    let names: Vec<&str> = spec
        .options
        .iter()
        .flat_map(|o| o.names.iter().map(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"-A"), "{names:?}");
    assert!(names.contains(&"-R"), "{names:?}");
    assert!(
        spec.subcommands.is_empty(),
        "ls has no subcommands: {:?}",
        spec.subcommands
    );
}

/// End to end through the registry: a stem no layer has gets derived
/// on the populator thread and lands on a later lookup. The first
/// lookup may legitimately return nothing — running a command is far
/// past the keystroke budget, so it is deliberately deferred.
#[test]
fn registry_derives_a_stem_no_layer_has() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bundled = tmp.path().join("specs");
    let derived = tmp.path().join("derived");
    std::fs::create_dir_all(&bundled).expect("mkdir");
    std::fs::write(bundled.join("git.json"), r#"{"name":"git"}"#).expect("write");

    // The command must be on PATH for the registry path, which resolves
    // by name. Put the fixture bin dir first for this process only.
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir");
    fake_bin(&bin_dir, "derivable", HELP);
    let _guard = PathGuard::prepend(&bin_dir);

    let registry =
        SpecRegistry::at_dirs_deriving(&[bundled.clone(), derived.clone()], Some(derived.clone()));

    let spec = lookup_until(&registry, "derivable").expect("derived spec lands");
    let subs: Vec<&str> = spec.subcommands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(subs, ["build", "clean", "publish"]);
    assert!(derived.join("derivable.json").exists());

    // A stem an existing layer already serves is never derived.
    assert!(registry.lookup("git").is_some());
    assert!(
        !derived.join("git.json").exists(),
        "git must not be derived"
    );
}

/// With no derivation target the registry behaves exactly as before:
/// an unknown stem stays unknown and nothing is spawned.
#[test]
fn registry_without_a_target_never_derives() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bundled = tmp.path().join("specs");
    let derived = tmp.path().join("derived");
    std::fs::create_dir_all(&bundled).expect("mkdir");

    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir");
    fake_bin(&bin_dir, "offlimits", HELP);
    let _guard = PathGuard::prepend(&bin_dir);

    let registry = SpecRegistry::at_dirs(&[bundled]);
    for _ in 0..5 {
        assert!(registry.lookup("offlimits").is_none());
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!derived.exists(), "nothing may be written");
}

/// A file that exists but does not parse is a negative entry for that
/// stem — it must not be silently replaced by a derived guess.
#[test]
fn a_broken_spec_file_is_not_overwritten_by_derivation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bundled = tmp.path().join("specs");
    let derived = tmp.path().join("derived");
    std::fs::create_dir_all(&bundled).expect("mkdir");
    std::fs::write(bundled.join("broken.json"), "{ not json").expect("write");

    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir");
    fake_bin(&bin_dir, "broken", HELP);
    let _guard = PathGuard::prepend(&bin_dir);

    let registry =
        SpecRegistry::at_dirs_deriving(&[bundled, derived.clone()], Some(derived.clone()));
    for _ in 0..5 {
        assert!(registry.lookup("broken").is_none());
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        !derived.join("broken.json").exists(),
        "a broken file must stay the answer"
    );
}

/// Poll a lookup for up to ~2s. Derivation happens on a background
/// thread, so the spec arrives on a later call, not the first.
fn lookup_until(
    registry: &SpecRegistry,
    name: &str,
) -> Option<std::sync::Arc<nerv_engine::spec_parser::Subcommand>> {
    for _ in 0..40 {
        if let Some(spec) = registry.lookup(name) {
            return Some(spec);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// `PATH` is process-global, so every test that changes it holds this
/// lock and restores the old value on drop.
struct PathGuard {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl PathGuard {
    fn prepend(dir: &Path) -> Self {
        let lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("PATH");
        let joined = match &previous {
            Some(p) => format!("{}:{}", dir.display(), p.to_string_lossy()),
            None => dir.display().to_string(),
        };
        // SAFETY: env mutation serialized by PATH_LOCK; `which` reads
        // PATH at call time.
        unsafe { std::env::set_var("PATH", joined) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

/// ANSI colour that survives `NO_COLOR` is stripped before parsing —
/// otherwise the escape bytes end up inside names and descriptions.
#[test]
fn ansi_colour_is_stripped_from_help() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let coloured = "\
Usage: painted <command>

\u{1b}[1mCommands:\u{1b}[0m
  \u{1b}[32mbuild\u{1b}[0m       Build the project
  \u{1b}[32mclean\u{1b}[0m       Remove build artifacts
";
    let bin = fake_bin(tmp.path(), "painted", coloured);
    let out_dir = tmp.path().join("derived");
    derive_from_binary("painted", &bin, &out_dir).expect("derived");

    let registry = SpecRegistry::at_dir(&out_dir);
    let spec = registry.lookup("painted").expect("spec loads");
    let subs: Vec<&str> = spec.subcommands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(subs, ["build", "clean"], "escape bytes leaked into names");
}
