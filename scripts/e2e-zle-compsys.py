#!/usr/bin/env python3
"""E2E: the ZLE popup falls back to zsh's own completion function.

`nervfx` (scripts/fixtures/compsys/bin) has no nerv spec, but a zsh
completion function `_nervfx` sits in fpath. The engine answers
`_complete --compsys` with exit 5 for it, and the widget then runs the
function in-process with `compadd` intercepted, showing what it offers
in the popup (docs/spec-conversion-policy.md §6.4).

Each scenario gets a fresh zsh on a pty rendered through pyte. The
fixture function appends a line to $HOME/calls on every run, so the
harness can tell whether the capture ran at all.

Run from repo root:
  cargo build -p nerv-cli && uv run -q --with pyte python3 scripts/e2e-zle-compsys.py
Requires: cargo-built debug binaries (default features), zsh, pyte.

The widget is baked into the binary with include_str! (nerv-cli/src/main.rs),
so editing _nerv.zsh changes nothing here until you rebuild.
"""
import os, shutil, signal, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from zle_harness import PROMPT, Shell  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
FIXTURE = os.path.join(REPO, "scripts", "fixtures", "compsys")


def log(msg):
    print(f"[e2e-compsys] {msg}", flush=True)


def boxed(sh, word):
    return any(word in l for l in sh.box())


def calls(home):
    try:
        with open(os.path.join(home, "calls")) as f:
            return len(f.readlines())
    except FileNotFoundError:
        return 0


def check_popup(env, home):
    """`nervfx ` lists the function's commands with their descriptions."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx ")
        sh.settle(lambda: boxed(sh, "alpha") and boxed(sh, "bravo"))
        # Once each: `_describe` also calls compadd with -D to filter,
        # and recording those calls would list every command twice.
        ok = sum(l.count("alpha") for l in sh.box()) == 1 and boxed(sh, "bravo")
        # Mid-token the first match is selected and its description is
        # the footer: the ` -- ` text `_describe` padded, parsed back out.
        sh.type("a")
        sh.settle(lambda: boxed(sh, "first fixture command"))
        ok = ok and boxed(sh, "first fixture command") and not boxed(sh, "bravo")
        if not ok:
            log(f"box: {sh.box()}")
        return ok
    finally:
        sh.close()


def check_accept(env, home):
    """Accepting a captured row inserts it like any other row."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx a")
        sh.settle(lambda: boxed(sh, "alpha"))
        # Without the popup, Tab falls through to zsh's own completion,
        # which would insert the same word and prove nothing.
        shown = boxed(sh, "alpha")
        # `alpha` is the only match, so zsh itself would insert it here if
        # the capture let it: the cursor must still be right after `a`.
        untouched = sh.at_typed("nervfx a")
        sh.type("\t")
        sh.settle(lambda: sh.cursor_line() == f"{PROMPT}nervfx alpha")
        ok = shown and untouched and sh.cursor_line() == f"{PROMPT}nervfx alpha"
        if not ok:
            log(f"shown: {shown}  untouched: {untouched}  line: {sh.cursor_line()!r}")
        return ok
    finally:
        sh.close()


def check_split(env, home):
    """A value zsh completes after an ignored prefix (`--mode=` + `fast`)
    is inserted as the whole token."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx --mode=f")
        sh.settle(lambda: boxed(sh, "fast"))
        shown = boxed(sh, "fast")   # see check_accept
        sh.type("\t")
        want = f"{PROMPT}nervfx --mode=fast"
        sh.settle(lambda: sh.cursor_line() == want)
        ok = shown and sh.cursor_line() == want
        if not ok:
            log(f"shown: {shown}  line: {sh.cursor_line()!r}  box: {sh.box()}")
        return ok
    finally:
        sh.close()


def check_clean(env, home):
    """The capture inserts nothing and lists nothing: the typed line is
    unchanged and no candidate appears outside the popup."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx ")
        sh.settle(lambda: boxed(sh, "alpha"))
        line_ok = sh.cursor_line() == f"{PROMPT}nervfx"
        leaked = [l for l in sh.outside_box() if "alpha" in l or "bravo" in l]
        ok = line_ok and not leaked and calls(home) >= 1
        if not ok:
            log(f"line: {sh.cursor_line()!r}  leaked: {leaked}  calls: {calls(home)}")
        return ok
    finally:
        sh.close()


def check_odd_rows(env, home):
    """A candidate with a tab stays one row, and one with no description
    gets an empty footer rather than a neighbour's text."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx ")
        sh.settle(lambda: boxed(sh, "odd name"))
        odd = boxed(sh, "odd name")
        sh.type("c")
        sh.settle(lambda: boxed(sh, "charlie"))
        ok = odd and boxed(sh, "charlie") and not boxed(sh, "fixture")
        if not ok:
            log(f"odd: {odd}  box: {sh.box()}")
        return ok
    finally:
        sh.close()


def check_quoted(env, home):
    """A file name zsh passes pre-quoted (`_path_files` uses compadd -Q) is
    inserted quoted once, not twice."""
    open(os.path.join(home, "My File"), "w").close()
    sh = Shell(env, home)
    try:
        sh.type("nervfx alpha My")
        sh.settle(lambda: boxed(sh, "My File"))
        shown = boxed(sh, "My File")
        sh.type("\t")
        want = f"{PROMPT}nervfx alpha My\\ File"
        sh.settle(lambda: sh.cursor_line() == want)
        ok = shown and sh.cursor_line() == want
        if not ok:
            log(f"shown: {shown}  line: {sh.cursor_line()!r}  box: {sh.box()}")
        return ok
    finally:
        sh.close()
        os.remove(os.path.join(home, "My File"))


def check_error(env, home):
    """A completion function that errors changes nothing: no insert, no
    error text on screen, and our compadd override is gone afterwards."""
    sh = Shell(dict(env, NERVFX_ERR="1"), home)
    try:
        sh.type("nervfx a")
        sh.settle(lambda: calls(home) >= 1, seconds=2.0)
        ran = calls(home) >= 1
        untouched = sh.at_typed("nervfx a")
        noise = [l for l in sh.screen.display if "fixture error" in l]
        # precmd records whether a `compadd` function is still defined.
        left = os.path.join(home, "compadd-left")
        try:
            os.remove(left)
        except FileNotFoundError:
            pass
        # Esc closes the popup (Enter would insert its row), ^U empties the
        # line, and running the empty line runs precmd. Not ^C: the pty
        # has no controlling terminal, so it sends no SIGINT.
        sh.type("\x1b")
        sh.settle(seconds=0.3)
        sh.type("\x15\r")
        sh.settle(lambda: os.path.exists(left), seconds=2.0)
        leftover = open(left).read().strip() if os.path.exists(left) else "missing"
        ok = ran and untouched and not noise and leftover == "0"
        if not ok:
            log(f"ran: {ran}  untouched: {untouched}  line: {sh.cursor_line()!r}"
                f"  noise: {noise}  compadd left: {leftover!r}")
            log("screen: " + " | ".join(l.rstrip() for l in sh.screen.display if l.strip()))
        return ok
    finally:
        sh.close()


def check_cache(env, home):
    """Typing on inside the same word filters the rows already captured:
    the function runs once for `nervfx `, not once per key."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx ")
        sh.settle(lambda: boxed(sh, "alpha"))
        sh.type("alp")
        sh.settle(lambda: boxed(sh, "alpha") and not boxed(sh, "bravo"))
        ok = boxed(sh, "alpha") and not boxed(sh, "bravo") and calls(home) == 1
        if not ok:
            log(f"calls: {calls(home)}  box: {sh.box()}")
        return ok
    finally:
        sh.close()


def check_no_match(env, home):
    """A word nothing matches stays empty; it does not re-run the function
    on every key (`a`, `ab`, `abc` against alpha/bravo/charlie)."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx abc")
        sh.settle(seconds=1.0)
        ok = calls(home) == 1
        if not ok:
            log(f"calls: {calls(home)}")
        return ok
    finally:
        sh.close()


def check_matcher(env, home):
    """With a matcher-list the user's zsh matches more than a plain prefix
    (oh-my-zsh sets case-insensitive): a word the saved rows don't match
    by prefix is asked again, so `ALP` still finds `alpha`."""
    sh = Shell(dict(env, NERVFX_MATCHER="1"), home)
    try:
        sh.type("nervfx ALP")
        sh.settle(lambda: boxed(sh, "alpha"))
        found = boxed(sh, "alpha")
        # While the matcher is what matches, each key asks zsh again, as
        # Tab would. A word nothing matches asks once and then stays
        # empty: `QQ` adds at most one call, not one per key.
        before = calls(home)
        sh.type("QQQ")
        sh.settle(seconds=1.0)
        ok = found and calls(home) - before <= 1
        if not ok:
            log(f"found: {found}  calls: {before} -> {calls(home)}")
        return ok
    finally:
        sh.close()


def check_new_prompt(env, home):
    """Rows captured before a command ran are not reused after it: the
    command may have changed what completes."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx ")
        sh.settle(lambda: boxed(sh, "alpha"))
        sh.type("\x1b")                     # close the popup
        sh.settle(seconds=0.3)
        sh.type("\x15\r")                   # empty line: a new prompt
        sh.settle(seconds=0.5)
        sh.type("nervfx ")
        sh.settle(lambda: calls(home) >= 2, seconds=2.0)
        ok = calls(home) == 2
        if not ok:
            log(f"calls: {calls(home)}")
        return ok
    finally:
        sh.close()


def check_suffix(env, home):
    """An option taking a value keeps its `=` and gets no space after it."""
    sh = Shell(env, home)
    try:
        sh.type("nervfx --mo")
        sh.settle(lambda: boxed(sh, "--mode"))
        shown = boxed(sh, "--mode")
        sh.type("\t")
        # The cursor lands right after `=`; the values popup that opens
        # next may paint a grey `fast` after it, so the line text can't say.
        sh.settle(lambda: sh.at_typed("nervfx --mode="))
        ok = shown and sh.at_typed("nervfx --mode=") \
            and sh.cursor_line().startswith(f"{PROMPT}nervfx --mode=")
        if not ok:
            log(f"shown: {shown}  line: {sh.cursor_line()!r}  x: {sh.screen.cursor.x}")
        return ok
    finally:
        sh.close()


def check_slow(env, home):
    """A command whose capture is slow stops being captured: the first
    call may be slow (autoload, cold caches), a later one may not."""
    sh = Shell(dict(env, NERVFX_SLOW="1"), home)
    try:
        sh.type("nervfx ")
        sh.settle(lambda: boxed(sh, "alpha"))
        first = boxed(sh, "alpha")          # 0.4 s on the first call: kept
        sh.type("alpha ")
        sh.settle(lambda: calls(home) >= 2, seconds=3.0)
        sh.settle(seconds=1.0)               # 0.4 s again: now slow
        sh.type("x ")
        sh.settle(seconds=1.5)
        ok = first and calls(home) == 2
        if not ok:
            log(f"first: {first}  calls: {calls(home)}")
        return ok
    finally:
        sh.close()


def check_off(env, home):
    """NERV_COMPSYS=0 turns the fallback off entirely."""
    sh = Shell(dict(env, NERV_COMPSYS="0"), home)
    try:
        sh.type("nervfx ")
        sh.settle(seconds=1.5)
        ok = not boxed(sh, "alpha") and calls(home) == 0
        if not ok:
            log(f"calls: {calls(home)}  box: {sh.box()}")
        return ok
    finally:
        sh.close()


def check_dir(env, home):
    """A directory keeps the `/` zsh would have added, so the next Tab
    goes inside it instead of past it."""
    os.makedirs(os.path.join(home, "subdir", "inner"), exist_ok=True)
    try:
        ok = True
        # Top level, then one level down: `_path_files` splits a nested
        # path between -p and -W.
        for typed, want in [("nervfx alpha subd", "nervfx alpha subdir/"),
                            ("nervfx alpha subdir/in", "nervfx alpha subdir/inner/")]:
            sh = Shell(env, home)
            try:
                sh.type(typed)
                name = want.rstrip("/").rsplit("/", 1)[-1].split()[-1]
                sh.settle(lambda: boxed(sh, name))
                shown = boxed(sh, name)
                sh.type("\t")
                # The cursor sits right after the `/` (the popup for the
                # directory's contents may ghost a name after it).
                done = lambda: sh.at_typed(want) and sh.cursor_line().startswith(f"{PROMPT}{want}")
                sh.settle(done)
                if not (shown and done()):
                    log(f"{typed!r}: shown: {shown}  line: {sh.cursor_line()!r}"
                        f"  x: {sh.screen.cursor.x}")
                    ok = False
            finally:
                sh.close()
        return ok
    finally:
        shutil.rmtree(os.path.join(home, "subdir"))


def check_spec_wins(env, home):
    """A command with a nerv spec never reaches compsys, even when a
    completion function is registered for it (`compdef _nervfx git`)."""
    sh = Shell(env, home)
    try:
        sh.type("git ")
        sh.settle(lambda: boxed(sh, "checkout"))
        ok = boxed(sh, "checkout") and not boxed(sh, "alpha") and calls(home) == 0
        if not ok:
            log(f"box: {sh.box()}  calls: {calls(home)}")
        return ok
    finally:
        sh.close()


SCENARIOS = [
    ("popup", check_popup),
    ("accept", check_accept),
    ("split", check_split),
    ("clean", check_clean),
    ("odd-rows", check_odd_rows),
    ("quoted", check_quoted),
    ("error", check_error),
    ("cache", check_cache),
    ("no-match", check_no_match),
    ("matcher", check_matcher),
    ("new-prompt", check_new_prompt),
    ("suffix", check_suffix),
    ("slow", check_slow),
    ("off", check_off),
    ("dir", check_dir),
    ("spec-wins", check_spec_wins),
]


def make_home():
    home = tempfile.mkdtemp(prefix="nerv-compsys-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write(f"PS1='{PROMPT}'\n")
        f.write(f"fpath=({FIXTURE} $fpath)\n")
        f.write(f"autoload -Uz compinit && compinit -u -d {home}/.zcompdump\n")
        f.write("compdef _nervfx git\n")
        f.write("[[ -n $NERVFX_MATCHER ]] && zstyle ':completion:*' matcher-list 'm:{a-zA-Z}={A-Za-z}'\n")
        f.write(f"precmd() {{ print -r -- ${{+functions[compadd]}} > {home}/compadd-left }}\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
    env = dict(os.environ)
    env.pop("TMUX", None)
    env.update(HOME=home, ZDOTDIR=zdot, NERV_SPECS_DIR=SPECS, NERV_AUTOSTART="0",
               NERV_PATH_SCAN="0", TERM="xterm-256color",
               PATH=os.path.join(FIXTURE, "bin") + os.pathsep + env.get("PATH", ""))
    return home, env


def wait_daemon(env):
    deadline = time.time() + 10
    while time.time() < deadline:
        out = subprocess.run([NERV, "_complete", "git c", "5"], env=env,
                             capture_output=True, text=True).stdout
        if "checkout" in out:
            return True
        time.sleep(0.2)
    return False


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2
    home, env = make_home()
    failed = []
    try:
        subprocess.run([NERV, "start"], env=env, capture_output=True)
        if not wait_daemon(env):
            log("FAIL — daemon never answered")
            return 1
        only = set(sys.argv[1:])
        for name, fn in SCENARIOS:
            if only and name not in only:
                continue
            try:
                os.remove(os.path.join(home, "calls"))
            except FileNotFoundError:
                pass
            ok = fn(env, home)
            log(f"{'PASS' if ok else 'FAIL'} {name}")
            if not ok:
                failed.append(name)
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        shutil.rmtree(home, ignore_errors=True)
    if failed:
        log(f"FAIL — {', '.join(failed)}")
        return 1
    log("PASS — all scenarios")
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    sys.exit(main())
