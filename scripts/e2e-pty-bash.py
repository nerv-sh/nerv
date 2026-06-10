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

        # 1. Ghost: partial command → dim remainder.
        os.write(master, TYPED.encode())
        out = drain(master, 2.0)
        ghost_ok = FAINT in out and GHOST in out
        log(f"ghost: FAINT={FAINT in out} GHOST={GHOST in out} -> {ghost_ok}")
        if not ghost_ok:
            log(f"  tail repr: {out[-300:]!r}")

        # 2. Accept the ghost with Right-arrow → daemon records the accept
        #    for frecency (mirrors the zsh harness).
        os.write(master, b"\x1b[C")
        drain(master, 1.0)
        frec = os.path.join(home, "Library", "Caches", "nerv", "frecency.tsv")
        frec_ok = False
        for _ in range(10):
            if os.path.exists(frec) and "checkout" in open(frec).read():
                frec_ok = True
                break
            time.sleep(0.2)
        log(f"frecency: recorded 'checkout' -> {frec_ok}")
        os.write(master, b"\x15")  # Ctrl-U clear line
        drain(master, 0.5)

        # 3. Popup: a prefix with >=2 completions (git c → checkout, commit)
        #    → boxed reverse-video list with a [1/2] footer.
        os.write(master, b"git c")
        out = drain(master, 2.0)
        REVERSE = b"\x1b[7m"
        BOX = "╭".encode()  # rounded box top-left
        popup_ok = REVERSE in out and b"[1/2]" in out and BOX in out
        log(f"popup: REVERSE={REVERSE in out} [1/2]={b'[1/2]' in out} BOX={BOX in out} -> {popup_ok}")
        if not popup_ok:
            log(f"  tail repr: {out[-400:]!r}")

        # 4. Navigation: Tab advances selection; footer → [2/2].
        os.write(master, b"\t")
        out = drain(master, 1.5)
        nav_ok = b"[2/2]" in out
        log(f"nav: [2/2]={nav_ok} -> {nav_ok}")

        # 5. PreExec: submitting a command emits the figterm PreExec marker
        #    (the gated DEBUG trap must fire on a real command, NOT during
        #    startup — the latter would have already broken the ghost).
        os.write(master, b"\x15")  # clear the popup line first
        drain(master, 0.3)
        os.write(master, b"echo hi\r")
        submit = drain(master, 1.5)
        preexec_ok = b"\x1b]697;PreExec\x07" in submit
        log(f"preexec on submit: {preexec_ok}")

        if ghost_ok and frec_ok and popup_ok and nav_ok and preexec_ok:
            log("PASS — bash PTY ghost + accept/frecency + popup + nav + preexec")
            rc = 0
        else:
            log("FAIL — see per-check output above")

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
