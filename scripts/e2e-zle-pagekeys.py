#!/usr/bin/env python3
"""E2E smoke for PageUp/PageDown popup paging (_nerv.zsh).

Types `brew ` (20 subcommands in the fixture, window of 10 on a 40-row
terminal), then sends PageDown and asserts the `[k/total]` footer jumps
by one window (1 -> 11) instead of one row, and PageUp returns to 1.

Run from repo root:  python3 scripts/e2e-zle-pagekeys.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import os
import pty
import re
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

PAGE_UP = b"\x1b[5~"
PAGE_DOWN = b"\x1b[6~"


def log(msg):
    print(f"[e2e-pagekeys] {msg}", flush=True)


def pump(fd, seconds):
    import select

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


def footers(raw):
    """All [k/total] footer counters in the raw output, in order."""
    return [(int(a), int(b)) for a, b in re.findall(rb"\[(\d+)/(\d+)\]", raw)]


def sentinel_counters(raw):
    """Sentinel-selected footers render as a bare `[N]` (item count, no
    slash). `\\[(\\d+)\\]` only matches that form — `[k/total]` has a
    slash before the `]`, so it's excluded."""
    return [int(m) for m in re.findall(rb"\[(\d+)\]", raw)]


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-pagekeys-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
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
        master, slave = pty.openpty()
        # 40 rows -> MAX_VIS clamps to 10; brew has 20 subcommands.
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

        pump(master, 2.0)  # reach first prompt

        os.write(master, b"brew ")
        out = pump(master, 2.0)
        # Popup opens on the "Immediately execute" sentinel (index 0), so
        # the footer is a bare `[N]` item count, not `[k/total]`.
        initial = sentinel_counters(out)
        log(f"after 'brew ': sentinel counters={initial[-3:]}")
        if not initial or initial[-1] < 12:
            log("FAIL — popup did not open on the sentinel with N >= 12")
            log(f"  tail repr: {out[-400:]!r}")
            return 1
        total = initial[-1]

        os.write(master, PAGE_DOWN)
        out = pump(master, 1.5)
        after_down = footers(out)
        log(f"after PageDown: footers={after_down[-3:]}")

        os.write(master, PAGE_UP)
        out = pump(master, 1.5)
        after_up = sentinel_counters(out)
        log(f"after PageUp: sentinel counters={after_up[-3:]}")

        # PageDown from the sentinel jumps one window (10) → item 10;
        # PageUp returns to the sentinel (bare [N]).
        down_ok = bool(after_down) and after_down[-1] == (10, total)
        up_ok = bool(after_up) and after_up[-1] == total
        if down_ok and up_ok:
            log(f"PASS — sentinel → PageDown [10/{total}] → PageUp [{total}]")
            rc = 0
        else:
            log(f"FAIL — down_ok={down_ok} up_ok={up_ok}")

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
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
