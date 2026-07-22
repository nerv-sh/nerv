#!/usr/bin/env python3
"""E2E regression for the ZLE popup on a NARROW terminal (_nerv.zsh).

On a small window (e.g. 38 cols) every painted popup row must fit inside
the terminal width; a row wider than COLUMNS is wrapped by the real
terminal, which tears the box apart (broken borders, orphan fragments —
the "terminal too small" breakage). The widget paints rows as
`ESC[B ESC[<col>G <content> ESC[K`, so the invariant is checkable from
the raw pty stream: col - 1 + visible_cells(content) <= COLS.

Run from repo root:  python3 scripts/e2e-zle-narrow.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import os
import pty
import re
import signal
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")

COLS = 38             # narrow window from the bug report screenshot
ROWS = 24

CSI_RE = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]")
ROW_RE = re.compile(rb"\x1b\[B\x1b\[(\d+)G(.*?)\x1b\[K", re.DOTALL)
BOX_GLYPHS = ("│", "╭", "╰", "├")


def log(msg):
    print(f"[e2e-narrow] {msg}", flush=True)


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
    return out


def visible_cells(raw):
    """Display-cell count of a painted row, ANSI stripped.

    Fixture content is ASCII + 1-cell box glyphs (│ ╭ ─ ↩ …), so cells ==
    codepoints after stripping escape sequences.
    """
    text = CSI_RE.sub(b"", raw).decode("utf-8", "replace")
    return len(text), text


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    # Short prefix: the daemon's UDS lives under $HOME and macOS caps
    # sun_path at 104 bytes — a long tempdir name breaks the socket.
    home = tempfile.mkdtemp(prefix="nerv-nw-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%% '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = SPECS
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    # Poll the IPC bridge until the daemon actually serves — a fixed
    # sleep races the socket bind and the widget then shows E1 instead
    # of the popup, which would false-fail this test.
    for _ in range(50):
        probe = subprocess.run(
            [NERV, "_complete", "git c", "5"], env=env, capture_output=True
        )
        if probe.returncode == 0:
            break
        time.sleep(0.1)
    else:
        log("FAIL — daemon never became reachable")
        return 2

    rc = 1
    try:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
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

        pump(master, 2.0)                      # reach first prompt

        os.write(master, b"git c")             # >=2 completions -> popup
        out = pump(master, 2.0)

        box_rows = []
        for m in ROW_RE.finditer(out):
            col = int(m.group(1))
            cells, text = visible_cells(m.group(2))
            if any(g in text for g in BOX_GLYPHS):
                box_rows.append((col, cells, text))

        if len(box_rows) < 3:
            log(f"FAIL — popup did not render (box rows={len(box_rows)})")
            log(f"  tail repr: {out[-400:]!r}")
            return 1

        overflow = [(c, w, t) for (c, w, t) in box_rows if c - 1 + w > COLS]
        widest = max(c - 1 + w for (c, w, _) in box_rows)
        log(f"box rows={len(box_rows)}  widest right edge={widest}  cols={COLS}")

        if overflow:
            c, w, t = overflow[0]
            log(f"FAIL — {len(overflow)} row(s) exceed the terminal width")
            log(f"  first: col={c} cells={w} right_edge={c - 1 + w} > {COLS}")
            log(f"  row: {t!r}")
        else:
            log("PASS — every popup row fits inside the narrow terminal")
            rc = 0

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
