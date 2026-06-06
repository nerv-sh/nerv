#!/usr/bin/env python3
"""End-to-end smoke for PTY-mode inline ghost text (Phase 3a).

Drives the real `nerv-pty` binary inside a pseudo-terminal, types a
partial command, and asserts the daemon-backed ghost escape sequence
shows up in the wrapper's output. This is the only layer that can verify
the full chain: shadow-terminal prompt detection (OSC 697 markers from
_nerv-pty.zsh) -> edit-buffer extraction -> nervd query -> ghost render.

Run from the repo root:  python3 scripts/e2e-pty-ghost.py
Requires: cargo-built debug binaries, a `zsh` on PATH.
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
PTY_ZSH = os.path.join(REPO, "shell-integrations", "zsh", "_nerv-pty.zsh")

# What we type and what the daemon should complete it to.
TYPED = "git che"
GHOST = b"ckout"            # remainder of "checkout"
FAINT = b"\x1b[2m"          # ghost is drawn in faint SGR
SAVE = b"\x1b7"             # DECSC precedes the ghost


def log(msg):
    print(f"[e2e] {msg}", flush=True)


def drain(fd, seconds):
    """Read whatever the pty emits for `seconds`, return raw bytes."""
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
    if not os.path.exists(PTY_ZSH):
        log(f"missing shell integration: {PTY_ZSH}")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-e2e-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    # Inner-shell rc: deterministic prompt, then load the PTY markers.
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='> '\nPROMPT='> '\n")
        f.write(f"source {PTY_ZSH}\n")

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["SHELL"] = "/bin/zsh"
    env["NERV_SPECS_DIR"] = SPECS
    # Make sure no stale session id leaks in as the wrapper.
    env.pop("NERV_PTY_SESSION_ID", None)

    # 1. Start the daemon with the git fixture loaded.
    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    rc = 1
    try:
        # 2. Spawn nerv-pty attached to a pty we control. Set a real
        #    window size first — the shadow terminal's grid panics on a
        #    0-row pty (storage.rs visible_lines assertion). No setsid:
        #    nerv-pty does its own session setup for the inner shell, and
        #    making the wrapper a session leader breaks that with EPERM.
        master, slave = pty.openpty()
        winsize = struct.pack("HHHH", 24, 80, 0, 0)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, winsize)
        proc = subprocess.Popen(
            [NERV_PTY, "--", "/bin/zsh"],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            close_fds=True,
        )
        os.close(slave)

        # 3. Let the inner shell reach its first prompt.
        startup = drain(master, 2.0)
        log(f"startup bytes: {len(startup)}")

        # 4. Type the partial command (no newline) and wait for the ghost.
        os.write(master, TYPED.encode())
        out = drain(master, 2.0)

        # 5. Assert the ghost render sequence is present.
        has_faint = FAINT in out
        has_ghost = GHOST in out
        has_save = SAVE in out
        log(f"saw SAVE={has_save} FAINT={has_faint} GHOST={has_ghost!r}={has_ghost}")
        if has_faint and has_ghost:
            log("PASS — inline ghost text rendered end-to-end")
            rc = 0
        else:
            log("FAIL — ghost not found in pty output")
            # Dump a trimmed view to aid debugging.
            tail = out[-400:]
            log(f"tail repr: {tail!r}")

        # 6. Tear down the shell.
        try:
            os.write(master, b"\x03")          # Ctrl-C to drop the line
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
