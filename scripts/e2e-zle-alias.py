#!/usr/bin/env python3
"""E2E smoke for widget-side alias expansion (_nerv.zsh).

With `alias g=git`, typing `g co` must surface the git spec's
subcommands (checkout/commit) — the widget rewrites the leading
alias through zsh's $aliases table before the IPC call, since the
engine keys specs on the literal first word.

Also asserts the guard: an alias body with metacharacters is NOT
expanded (no popup rather than a broken one).

Run from repo root:  python3 scripts/e2e-zle-alias.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import os
import pty
import select
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

DSR_QUERY = b"\x1b[6n"
DSR_REPLY = b"\x1b[1;20R"


def log(msg):
    print(f"[e2e-alias] {msg}", flush=True)


def pump(fd, seconds):
    out = b""
    deadline = time.time() + seconds
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.1)
        if fd in r:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
            if DSR_QUERY in chunk:
                os.write(fd, DSR_REPLY)
    return out


def spawn_shell(env):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 200, 0, 0))
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
    return proc, master


def teardown(proc, master):
    try:
        os.write(master, b"\x03exit\n")
    except OSError:
        pass
    time.sleep(0.3)
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
    os.close(master)


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-alias-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='> '\n")
        f.write("alias g=git\n")
        f.write("alias weird='git status; echo hi'\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = SPECS
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    rc = 1
    try:
        proc, master = spawn_shell(env)
        pump(master, 2.0)
        os.write(master, b"g co")
        out = pump(master, 2.5)
        expanded = b"checkout" in out and b"commit" in out
        log(f"alias g=git: checkout={b'checkout' in out} commit={b'commit' in out}")
        teardown(proc, master)

        proc, master = spawn_shell(env)
        pump(master, 2.0)
        os.write(master, b"weird co")
        out2 = pump(master, 2.0)
        # Metachar body must NOT be expanded into git suggestions.
        guarded = b"checkout" not in out2
        log(f"metachar alias guarded (no git popup): {guarded}")
        teardown(proc, master)

        if expanded and guarded:
            log("PASS — alias expands to its spec, unsafe bodies left alone")
            rc = 0
        else:
            log("FAIL")
            log(f"  tail1: {out[-300:]!r}")
            log(f"  tail2: {out2[-300:]!r}")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
