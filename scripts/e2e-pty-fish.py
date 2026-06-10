#!/usr/bin/env python3
"""End-to-end smoke for PTY-mode inline ghost text under **fish**.

Like bash, fish reaches inline autocomplete only through the PTY path
(PLAN §6.2). Drives `nerv-pty` wrapping `fish`, sources `_nerv-pty.fish`
(OSC 697 markers via the fish_prompt event + prompt wrap), types a
partial command, and asserts the daemon-backed ghost shows up.

**Skips cleanly when fish is not installed** — fish isn't on every dev
machine, so this is opportunistic verification, not a hard gate.

Run from the repo root:  python3 scripts/e2e-pty-fish.py
"""

import fcntl
import os
import pty
import re
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
NERV_PTY = os.path.join(REPO, "target", "debug", "nerv-pty")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
PTY_FISH = os.path.join(REPO, "shell-integrations", "fish", "_nerv-pty.fish")

TYPED = "git che"
GHOST = b"ckout"
FAINT = b"\x1b[2m"


def log(msg):
    print(f"[e2e] {msg}", flush=True)


def respond_to_queries(fd, buf):
    """Answer the terminal-capability queries fish 4.x blocks on at
    startup. A real terminal replies to these; our bare pty must emulate
    them or fish never reaches its prompt (unlike bash/zsh, which don't
    probe). Minimal/negative answers are enough to unblock fish."""
    if b"\x1b]11;?" in buf:  # OSC 11 background-color query
        os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
    for _ in re.findall(rb"\x1bP\+q[0-9a-fA-F]*\x1b\\", buf):  # XTGETTCAP
        os.write(fd, b"\x1bP0+r\x1b\\")  # "unsupported"
    if b"\x1b[?u" in buf:  # kitty keyboard progressive-enhancement query
        os.write(fd, b"\x1b[?0u")
    if re.search(rb"\x1b\[\d*c", buf):  # primary device attributes
        os.write(fd, b"\x1b[?62;c")


def drain(fd, seconds, answer=False):
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
            if answer:
                respond_to_queries(fd, chunk)
    return out


def main():
    fish = shutil.which("fish")
    if not fish:
        log("fish not installed — skipping (brew install fish to run this)")
        return 0
    for path in (NERV, NERV_PTY):
        if not os.path.exists(path):
            log(f"missing binary: {path} — run `cargo build -p nerv-cli -p nerv-pty`")
            return 2
    if not os.path.exists(PTY_FISH):
        log(f"missing shell integration: {PTY_FISH}")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-e2e-fish-")
    env = dict(os.environ)
    env["HOME"] = home
    env["SHELL"] = fish
    env["NERV_SPECS_DIR"] = SPECS
    env.pop("NERV_PTY_SESSION_ID", None)

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    rc = 1
    try:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        # Define a deterministic prompt, then source the markers. Order
        # matters: the bootstrap copies fish_prompt at source time.
        proc = subprocess.Popen(
            [
                NERV_PTY, "--", fish,
                # Deterministic prompt + disable fish's own grey
                # autosuggestion so the only ghost we can see is nerv's
                # faint one. Source the markers last.
                "-C", "function fish_prompt; printf '> '; end",
                "-C", "set -g fish_autosuggestion_enabled 0",
                "-C", f"source {PTY_FISH}",
                "-i",
            ],
            stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True,
        )
        os.close(slave)

        startup = drain(master, 2.5, answer=True)
        log(f"startup bytes: {len(startup)}  OSC697={b'697' in startup}")

        # 1. Ghost.
        os.write(master, TYPED.encode())
        out = drain(master, 2.0, answer=True)
        ghost_ok = FAINT in out and GHOST in out
        log(f"ghost: FAINT={FAINT in out} GHOST={GHOST in out} -> {ghost_ok}")
        if not ghost_ok:
            log(f"  tail repr: {out[-300:]!r}")

        # 2. Accept (Right-arrow) → frecency.
        os.write(master, b"\x1b[C")
        drain(master, 1.0, answer=True)
        frec = os.path.join(home, "Library", "Caches", "nerv", "frecency.tsv")
        frec_ok = False
        for _ in range(10):
            if os.path.exists(frec) and "checkout" in open(frec).read():
                frec_ok = True
                break
            time.sleep(0.2)
        log(f"frecency: recorded 'checkout' -> {frec_ok}")
        os.write(master, b"\x15")  # Ctrl-U clear
        drain(master, 0.5, answer=True)

        # 3. Popup (git c → checkout, commit) → box + reverse + [1/2].
        os.write(master, b"git c")
        out = drain(master, 2.0, answer=True)
        REVERSE = b"\x1b[7m"
        BOX = "╭".encode()
        popup_ok = REVERSE in out and b"[1/2]" in out and BOX in out
        log(f"popup: REVERSE={REVERSE in out} [1/2]={b'[1/2]' in out} BOX={BOX in out} -> {popup_ok}")
        if not popup_ok:
            log(f"  tail repr: {out[-400:]!r}")

        # 4. Navigation: Tab → [2/2].
        os.write(master, b"\t")
        out = drain(master, 1.5, answer=True)
        nav_ok = b"[2/2]" in out
        log(f"nav: [2/2]={nav_ok} -> {nav_ok}")

        # 5. PreExec on submit (fish_preexec event).
        os.write(master, b"\x15")
        drain(master, 0.3, answer=True)
        os.write(master, b"echo hi\r")
        submit = drain(master, 1.5, answer=True)
        preexec_ok = b"\x1b]697;PreExec\x07" in submit
        log(f"preexec on submit: {preexec_ok}")

        if ghost_ok and frec_ok and popup_ok and nav_ok and preexec_ok:
            log("PASS — fish PTY ghost + accept/frecency + popup + nav + preexec")
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
