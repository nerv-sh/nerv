#!/usr/bin/env python3
"""E2E: the popup sits under the cursor on a line with Hangul in it (_nerv.zsh).

A Hangul character is one character and two cells. Counted as characters,
the line before the cursor came out short and the box was drawn that many
cells to the left of where the typing is; counted as bytes on the way to
the daemon, the line was cut short and nothing completed at all.

The box's left border has a fixed offset from the cursor. It is measured
on an ASCII line and must be the same on `git commit -m "한글 메시지" --`.

Run from repo root:  python3 scripts/e2e-zle-hangul.py
Requires: cargo-built debug binaries, zsh, and pyte.
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

try:
    import pyte
except ImportError:
    print("SKIP — pyte not installed (pip install pyte)")
    sys.exit(0)

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
ROWS, COLS = 40, 120


def log(msg):
    print(f"[e2e-hangul] {msg}", flush=True)


def box_offset(env, typed):
    """(left border column − cursor column, screen text) after typing."""
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
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)

    def pump(seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            r, _, _ = select.select([master], [], [], 0.05)
            if master in r:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                stream.feed(chunk)

    pump(2.0)
    os.write(master, typed.encode())
    pump(2.0)
    text = "\n".join(line.rstrip() for line in screen.display)
    offset = None
    for y in range(ROWS):
        for x in range(COLS):
            if screen.buffer[y][x].data == "╭":
                offset = x - screen.cursor.x
                break
        if offset is not None:
            break
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
    return offset, text


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-hangul-")
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
    env["LANG"] = env["LC_ALL"] = "en_US.UTF-8"
    env.pop("TERM_PROGRAM", None)

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)
    try:
        ascii_off, ascii_text = box_offset(env, "git commit --")
        hangul_off, hangul_text = box_offset(env, 'git commit -m "한글 메시지" --')
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        shutil.rmtree(home, ignore_errors=True)

    log(f"border − cursor: ascii={ascii_off} hangul={hangul_off}")
    checks = [
        ("ascii line: popup opens", ascii_off is not None),
        ("hangul line: popup opens with the option rows",
         hangul_off is not None and "--all" in hangul_text),
        ("hangul line: the box is under the cursor, as on the ascii line",
         ascii_off is not None and hangul_off == ascii_off),
    ]
    for name, ok in checks:
        log(f"{'OK  ' if ok else 'FAIL'} {name}")
    if all(ok for _, ok in checks):
        log("PASS — wide characters before the cursor do not move the popup")
        return 0
    log("hangul screen:\n" + hangul_text)
    log("FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
