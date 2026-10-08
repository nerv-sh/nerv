#!/usr/bin/env python3
"""E2E: Tab keeps working for the plugin that had it before nerv (_nerv.zsh).

nerv binds Tab and re-binds it on every prompt. A plugin that binds Tab
once (fzf-tab) used to lose the key for good. Now nerv remembers the
widget it takes Tab from and hands the key back whenever it has no rows.

Case 1 — rival bound before `nerv init`: nerv has rows → nerv inserts.
Case 2 — same shell, nothing to complete → the rival's widget runs.
Case 3 — rival bound late (after the first prompt, as zinit turbo or
  zsh-defer would): from the next prompt on it is the fallback.

Run from repo root:  python3 scripts/e2e-zle-tabowner.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")


def log(msg):
    print(f"[e2e-tabowner] {msg}", flush=True)


def pump(fd, seconds):
    deadline = time.time() + seconds
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.05)
        if fd in r:
            try:
                if not os.read(fd, 65536):
                    break
            except OSError:
                break


def buffer_after(env, home, steps):
    """Run `steps` (byte chunks, each given time to settle) in a fresh
    shell, then dump $BUFFER through a bound widget and return it."""
    out = os.path.join(home, "buffer.txt")
    if os.path.exists(out):
        os.remove(out)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
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
    pump(master, 2.0)
    for chunk in steps:
        os.write(master, chunk)
        pump(master, 1.2)
    os.write(master, b"\x18\x02")
    pump(master, 0.5)
    try:
        os.write(master, b"\x03\nexit\n")
    except OSError:
        pass
    time.sleep(0.2)
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
    os.close(master)
    try:
        with open(out) as f:
            return f.read()
    except OSError:
        return "<no buffer dump>"


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-tabowner-")
    out = os.path.join(home, "buffer.txt")
    rival = 'rival-tab() { LBUFFER+="<RIVAL>" }\nzle -N rival-tab\n'
    dump = (
        f'dump-buffer() {{ print -rn -- "$BUFFER" > {out} }}\n'
        "zle -N dump-buffer\nbindkey '^X^B' dump-buffer\n"
    )

    def zdot(name, body):
        d = os.path.join(home, name)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, ".zshrc"), "w") as f:
            f.write("PS1='%# '\n" + body + dump)
        return d

    early = zdot(
        "early",
        rival + "bindkey '^I' rival-tab\n" + f'eval "$({NERV} init zsh)"\n',
    )
    # Bound from a one-shot precmd that runs after nerv's own hooks on the
    # first prompt: nerv only sees it when the second prompt comes.
    late = zdot(
        "late",
        f'eval "$({NERV} init zsh)"\n'
        + rival
        + "late-bind() { bindkey '^I' rival-tab; add-zsh-hook -d precmd late-bind }\n"
        + "autoload -Uz add-zsh-hook; add-zsh-hook precmd late-bind\n",
    )

    env = dict(os.environ)
    env["HOME"] = home
    env["NERV_SPECS_DIR"] = SPECS
    env["TERM"] = "xterm-256color"
    env["ZDOTDIR"] = early

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    checks = []
    try:
        got = buffer_after(env, home, [b"git che", b"\t"])
        checks.append(("1 nerv has rows: nerv inserts", got, "git checkout "))

        got = buffer_after(env, home, [b"git checkout zz", b"\t"])
        checks.append(("2 nerv has none: the earlier owner runs", got, "git checkout zz<RIVAL>"))

        env["ZDOTDIR"] = late
        got = buffer_after(env, home, [b"\r", b"git checkout zz", b"\t"])
        checks.append(("3 an owner bound late is picked up", got, "git checkout zz<RIVAL>"))
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        shutil.rmtree(home, ignore_errors=True)

    ok = True
    for name, got, want in checks:
        passed = got == want
        ok = ok and passed
        log(f"{'OK  ' if passed else 'FAIL'} {name}" + ("" if passed else f": {got!r} (want {want!r})"))
    if ok and len(checks) == 3:
        log("PASS — Tab falls back to the widget that owned it")
        return 0
    log("FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
