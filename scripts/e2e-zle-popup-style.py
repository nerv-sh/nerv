#!/usr/bin/env python3
"""E2E regression for the ZLE popup's row and footer styling (_nerv.zsh).

Checks what a screenshot would show, from the raw pty stream:

  1. `git ` opens on the sentinel: its row is the selection bar (the
     default purple palette) and the footer shows the keys, not a
     second "Immediately execute". A list that fits shows no counter.
  2. Tab moves the bar to an item row (`›` inside the bar) and every box
     row keeps the same width.
  2b. After running `git status`, `git ` puts `status` — what the history
     ghost shows — first, not in its alphabetical place.
  3. `z vo` with long zoxide paths: the footer shows the path with its
     head cut at a folder boundary (`…/projects/archive/voucher-wiki`), never the `(score …)` suffix, and the
     path does not widen the box past its minimum. A short path under
     $HOME is shown as `~/…`. The typed `vo` is marked in the rows.

zoxide is a stub on PATH, so the test needs no real zoxide database.

Run from repo root:  python3 scripts/e2e-zle-popup-style.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import glob
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
FIXTURES = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")

COLS, ROWS = 120, 40
MIN_W = 46  # __NERV_WIDTH

CSI_RE = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]")
# The first row moves down past any wrapped ghost lines (`ESC[<n>B`).
ROW_RE = re.compile(rb"\x1b\[\d*B\x1b\[(\d+)G(.*?)\x1b\[K", re.DOTALL)
BAR = b"\x1b[0;48;5;134;38;5;255m"  # default (purple) selection bar
HL = b"\x1b[38;5;134m"              # default (purple) matched letters


def log(msg):
    print(f"[e2e-style] {msg}", flush=True)


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


def last_frame(out):
    """Rows of the last painted box: (raw, plain) from `╭` to `╰`."""
    rows = [m.group(2) for m in ROW_RE.finditer(out)]
    tops = [i for i, r in enumerate(rows) if "╭".encode() in r]
    if not tops:
        return []
    frame = []
    for raw in rows[tops[-1]:]:
        frame.append((raw, CSI_RE.sub(b"", raw).decode("utf-8", "replace").strip()))
        if "╰".encode() in raw:
            break
    return frame


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli -p nerv-daemon`")
        return 2

    # Short prefix: macOS caps the UDS path at 104 bytes.
    home = tempfile.mkdtemp(prefix="nerv-st-")
    zdot = os.path.join(home, "zdot")
    specs = os.path.join(home, "specs")
    stub = os.path.join(home, "bin")
    for d in (zdot, specs, stub):
        os.makedirs(d, exist_ok=True)
    for f in glob.glob(os.path.join(FIXTURES, "*.json")):
        shutil.copy(f, specs)
    with open(os.path.join(specs, "z.json"), "w") as f:
        f.write('{"name":"z","args":[{"name":"dir","generators":[{"type":"zoxide_query"}]}]}')

    long_path = f"{home}/workspace/lemoncloud/lemon/projects/archive/voucher-wiki"
    short_path = f"{home}/voucher-api"
    with open(os.path.join(stub, "zoxide"), "w") as f:
        f.write("#!/bin/sh\n")
        f.write(f"printf '  23.9 {long_path}\\n  14.1 {short_path}\\n'\n")
    os.chmod(os.path.join(stub, "zoxide"), 0o755)

    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%% '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = specs
    env["NERV_FRECENCY_FILE"] = "-"
    env["NERV_MISSES_FILE"] = "-"
    # No daemon history: its ranking would lift `status` by itself. With
    # it off, rows stay alphabetical and the ghost comes from zsh's own
    # $history, so only the widget's reorder can put `status` first.
    env["NERV_HISTORY_FILE"] = "-"
    env["PATH"] = stub + os.pathsep + env.get("PATH", "")
    env["TERM"] = "xterm-256color"
    env.pop("NERV_POPUP_THEME", None)

    subprocess.run([NERV, "start"], env=env, capture_output=True)
    for _ in range(50):
        if subprocess.run([NERV, "_complete", "git c", "5"], env=env,
                          capture_output=True).returncode == 0:
            break
        time.sleep(0.1)
    else:
        log("FAIL — daemon never became reachable")
        return 2

    failures = []

    def check(ok, what, frame=None):
        if ok:
            log(f"ok   {what}")
        else:
            failures.append(what)
            log(f"FAIL {what}")
            for _, plain in frame or []:
                log(f"     {plain}")

    try:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        proc = subprocess.Popen(["/bin/zsh"], preexec_fn=os.setsid, stdin=slave,
                                stdout=slave, stderr=slave, env=env, close_fds=True)
        os.close(slave)
        pump(master, 2.0)

        # 1. sentinel selected
        os.write(master, b"git ")
        raw = pump(master, 2.0)
        frame = last_frame(raw)
        # Every paint sequence must be a real escape: a quoting slip once
        # printed `\e[4G` as text above the box.
        check(b"\\e[" not in raw, "no escape sequence printed as text", frame)
        check(len(frame) >= 5, "popup opens on `git `", frame)
        sentinel = [raw for raw, plain in frame if "Immediately execute" in plain]
        check(len(sentinel) == 1 and BAR in sentinel[0],
              "sentinel row is the selection bar", frame)
        footer = frame[-2][1] if len(frame) >= 2 else ""
        check("enter run" in footer and "Immediately execute" not in footer,
              "sentinel footer shows keys, not the label again", frame)
        check("[" not in footer, "a list that fits shows no counter", frame)

        # 2. Tab onto an item
        os.write(master, b"\t")
        frame = last_frame(pump(master, 1.5))
        bar_rows = [raw for raw, _ in frame if BAR in raw]
        check(len(bar_rows) == 1 and (BAR + "›".encode()) in bar_rows[0],
              "Tab moves the bar to one item row with a › marker", frame)
        widths = {len(plain) for _, plain in frame}
        check(len(widths) == 1, "every box row has the same width", frame)

        # 2b. the ghost's next word leads the rows
        os.write(master, b"\x1b\x15")
        pump(master, 0.5)
        os.write(master, b"git status\r")
        pump(master, 1.5)
        os.write(master, b"git ")
        frame = last_frame(pump(master, 2.0))
        rows = [plain for _, plain in frame if plain.startswith("│")]
        first_item = rows[1] if len(rows) > 1 else ""
        check("status" in first_item, "the ghosted `status` row comes first", frame)

        # 3. zoxide paths in the footer
        os.write(master, b"\x1b\x15")          # Esc, then kill the line
        pump(master, 0.5)
        os.write(master, b"z ")
        pump(master, 1.0)
        os.write(master, b"vo")
        frame = last_frame(pump(master, 2.0))
        footer = frame[-2][1] if len(frame) >= 2 else ""
        check(len(frame) >= 4, "popup opens on `z vo`", frame)
        check(footer.startswith("│ …/") and "/archive/voucher-wiki " in footer,
              "long path keeps whole trailing folders after …/", frame)
        check(any(HL + b"vo" in raw for raw, _ in frame),
              "the typed `vo` is marked in the rows", frame)
        check("score" not in footer and home not in footer,
              "footer has no score and no raw $HOME", frame)
        top = frame[0][1] if frame else ""
        check(len(top) == MIN_W, f"path does not widen the box (width {len(top)})", frame)

        os.write(master, b"\x1b[B")           # Down (Tab would insert the row)
        frame = last_frame(pump(master, 1.5))
        footer = frame[-2][1] if len(frame) >= 2 else ""
        check("~/voucher-api" in footer, "short $HOME path shows as ~/", frame)

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
        shutil.rmtree(home, ignore_errors=True)

    if failures:
        log(f"FAIL — {len(failures)} check(s)")
        return 1
    log("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
