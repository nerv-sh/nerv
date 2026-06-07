#!/usr/bin/env python3
"""End-to-end smoke for PTY-mode inline ghost text under **bash**.

bash has no ZLE, so this is the only way inline autocomplete reaches it
(PLAN §6.2). Drives the real `nerv-pty` binary wrapping `/bin/bash`,
sourcing `_nerv-pty.bash` (OSC 697 markers via PROMPT_COMMAND + DEBUG
trap), types a partial command, and asserts the daemon-backed ghost
escape sequence shows up. The sibling `e2e-pty-ghost.py` covers zsh; this
proves the shell-agnostic PTY path also lights up bash.

Run from the repo root:  python3 scripts/e2e-pty-bash.py
Requires: cargo-built debug binaries, `/bin/bash`.
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
NERV_PTY = os.path.join(REPO, "target", "debug", "nerv-pty")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
PTY_BASH = os.path.join(REPO, "shell-integrations", "bash", "_nerv-pty.bash")
BASH = "/bin/bash"

TYPED = "git che"
GHOST = b"ckout"            # remainder of "checkout"
FAINT = b"\x1b[2m"          # ghost is drawn in faint SGR


def log(msg):
    print(f"[e2e] {msg}", flush=True)


def drain(fd, seconds):
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
    return out


def main():
    for path in (NERV, NERV_PTY):
        if not os.path.exists(path):
            log(f"missing binary: {path} — run `cargo build -p nerv-cli -p nerv-pty`")
            return 2
    if not os.path.exists(PTY_BASH):
        log(f"missing shell integration: {PTY_BASH}")
        return 2
    if not os.path.exists(BASH):
        log(f"no bash at {BASH} — skipping")
        return 0

    home = tempfile.mkdtemp(prefix="nerv-e2e-bash-")
    # Deterministic prompt + load the PTY markers. Interactive non-login
    # bash sources ~/.bashrc, which nerv-pty's exec'd shell will be.
    with open(os.path.join(home, ".bashrc"), "w") as f:
        f.write("PS1='> '\n")
        f.write(f"source {PTY_BASH}\n")

    env = dict(os.environ)
    env["HOME"] = home
    env["SHELL"] = BASH
    env["NERV_SPECS_DIR"] = SPECS
    env.pop("NERV_PTY_SESSION_ID", None)

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    rc = 1
    try:
        master, slave = pty.openpty()
        winsize = struct.pack("HHHH", 24, 80, 0, 0)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, winsize)
        # `--rcfile` forces our isolated rc even though the shell is
        # interactive; `-i` keeps it interactive inside the pty.
        proc = subprocess.Popen(
            [NERV_PTY, "--", BASH, "--rcfile", os.path.join(home, ".bashrc"), "-i"],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            close_fds=True,
        )
        os.close(slave)

        startup = drain(master, 2.0)
        log(f"startup bytes: {len(startup)}  OSC697={b'697' in startup}")

        os.write(master, TYPED.encode())
        out = drain(master, 2.0)
        ghost_ok = FAINT in out and GHOST in out
        log(f"ghost: FAINT={FAINT in out} GHOST={GHOST in out} -> {ghost_ok}")
        if not ghost_ok:
            log(f"  tail repr: {out[-300:]!r}")

        if ghost_ok:
            log("PASS — bash PTY ghost e2e")
            rc = 0
        else:
            log("FAIL — no ghost under bash; see tail above")

        try:
            os.write(master, b"\x03")
            os.write(master, b"exit\n")
        except OSError:
            pass
        time.sleep(0.3)
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
        os.close(master)
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
