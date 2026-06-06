#!/usr/bin/env python3
"""E2E smoke for the ZLE-widget popup column anchor (_nerv.zsh).

The widget anchors the popup's left edge under the input cursor by
asking the terminal for the cursor column via DSR (`ESC [ 6 n`). A plain
pty doesn't emulate a terminal, so this harness plays the terminal: it
answers the DSR with a chosen column, then asserts the widget paints the
popup at that column (`ESC [ <col> G`) rather than column 1.

Run from repo root:  python3 scripts/e2e-zle-popup.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import os
import pty
import re
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

DSR_COL = 50          # column we tell the widget the cursor is at
COLS = 200            # wide enough that the box won't be clamped left
DSR_QUERY = b"\x1b[6n"
DSR_REPLY = f"\x1b[1;{DSR_COL}R".encode()


def log(msg):
    print(f"[e2e-zle] {msg}", flush=True)


def pump(fd, seconds, on_dsr=None):
    """Read for `seconds`, answering DSR queries via `on_dsr`."""
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
            if on_dsr and DSR_QUERY in chunk:
                os.write(fd, DSR_REPLY)
    return out


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-zle-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    # Long prompt so a column-1 box would clearly be wrong.
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='this-is-a-long-test-prompt %~ %# '\n")
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
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, COLS, 0, 0))
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

        pump(master, 2.0, on_dsr=True)         # reach first prompt

        # Type a prefix with ≥2 completions (git c -> checkout, commit).
        os.write(master, b"git c")
        out = pump(master, 2.0, on_dsr=True)

        # The widget computes the cursor column from the (long) prompt
        # width + typed text, then paints each popup row with ESC[<col>G.
        # With a ~70-char prompt the anchor column must be well past 1.
        cols_used = [int(m) for m in re.findall(rb"\x1b\[(\d+)G", out)]
        popup_cols = [c for c in cols_used if c > 1]
        anchored = any(c >= 20 for c in cols_used)
        at_col1_only = bool(cols_used) and all(c <= 1 for c in cols_used)
        log(f"CHA cols={sorted(set(cols_used))}")
        log(f"anchored(>=20)={anchored}  column-1-only={at_col1_only}  popup_cols={popup_cols[:3]}")

        if anchored and not at_col1_only:
            log("PASS — popup anchored under the cursor column")
            rc = 0
        else:
            log("FAIL — popup not anchored to cursor column")
            log(f"  tail repr: {out[-400:]!r}")

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
