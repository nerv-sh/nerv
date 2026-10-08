#!/usr/bin/env python3
"""E2E: a terminal outside the matrix gets one honest line, once (_nerv.zsh).

docs/terminal-compat.md §1 lists what is tested. A terminal that names
itself through TERM_PROGRAM and is not on that list (Warp, VS Code, …)
used to degrade in silence.

1. TERM_PROGRAM=WarpTerminal: the first shell says so.
2. The next shell in the same terminal says nothing.
3. A different unlisted terminal (vscode) is told once too.
4. Listed terminals (iTerm.app, tmux) and an unnamed one: never.
5. With powerlevel10k loaded the line waits for the second prompt: its
   instant prompt faults anything printed before the first one.

Run from repo root:  python3 scripts/e2e-zle-terminal-notice.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import os
import pty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
NOTICE = "is outside the tested terminals"


def log(msg):
    print(f"[e2e-terminal-notice] {msg}", flush=True)


def first_screen(env, term_program, then=b""):
    """Everything a fresh interactive shell prints up to its first prompt,
    and after the keys in `then` if any."""
    env = dict(env)
    env.pop("TERM_PROGRAM", None)
    env.pop("TERMINAL_EMULATOR", None)
    if term_program is not None:
        env["TERM_PROGRAM"] = term_program
    master, slave = pty.openpty()
    proc = subprocess.Popen(
        ["/bin/zsh"],
        preexec_fn=os.setsid,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        close_fds=True,
    )
    os.close(slave)
    out = b""
    for keys in (b"", then):
        if keys:
            os.write(master, keys)
        elif out:
            break
        deadline = time.time() + 2.5
        while time.time() < deadline:
            r, _, _ = select.select([master], [], [], 0.05)
            if master in r:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                out += chunk
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
    os.close(master)
    return out.decode("utf-8", "replace")


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-termnotice-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_AUTOSTART"] = "0"
    env["TERM"] = "xterm-256color"

    checks = []
    try:
        out = first_screen(env, "WarpTerminal")
        checks.append(("1 unlisted terminal: told, by name",
                       out.count(NOTICE) == 1 and "[nerv] WarpTerminal " in out))
        checks.append(("2 same terminal again: silent",
                       NOTICE not in first_screen(env, "WarpTerminal")))
        out = first_screen(env, "vscode")
        checks.append(("3 another unlisted terminal: told once",
                       out.count(NOTICE) == 1 and NOTICE not in first_screen(env, "vscode")))
        checks.append(("3 the first one stays told",
                       NOTICE not in first_screen(env, "WarpTerminal")))
        for name in ("iTerm.app", "Apple_Terminal", "WezTerm", "tmux", None):
            checks.append((f"4 {name or 'unnamed'}: never",
                           NOTICE not in first_screen(env, name)))
        # A shell that has powerlevel10k (its `p10k` function) loaded.
        with open(os.path.join(zdot, ".zshrc"), "a") as f:
            f.write("p10k() { : }\n")
        checks.append(("5 p10k: silent at the first prompt",
                       NOTICE not in first_screen(env, "Hyper")))
        checks.append(("5 p10k: told at the second",
                       first_screen(env, "Hyper", then=b"\r").count(NOTICE) == 1))
    finally:
        shutil.rmtree(home, ignore_errors=True)

    for name, ok in checks:
        log(f"{'OK  ' if ok else 'FAIL'} {name}")
    if len(checks) == 11 and all(ok for _, ok in checks):
        log("PASS — unlisted terminals are told once, listed ones never")
        return 0
    log("FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
