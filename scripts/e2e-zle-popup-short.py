#!/usr/bin/env python3
"""E2E: ZLE popup must not tear in a SHORT terminal window.

A long prompt anchors the box far right; the painted footprint is 2
(lead) + W cells, so if the right-edge clamp under-reserves, the border
autowraps and the box tears into margin fragments. A short window with a
wrapped prompt can also overflow the bottom. This harness drives the
widget under pyte across several window heights and FAILS (exit 1) if any
box-border glyph lands in the far-left margin (a torn fragment).

Run from repo root:  python3 scripts/repro-short-popup.py
Env: ROWS / COLS override the single-shot window (no sweep).
Requires: cargo-built debug binaries, zsh, and pyte.
"""
import fcntl, os, pty, select, signal, struct, subprocess, sys, tempfile, termios, time
try:
    import pyte
except ImportError:
    print("SKIP — pyte not installed (pip install pyte)")
    sys.exit(0)

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")

ROWS = int(os.environ.get("ROWS", "14"))
COLS = int(os.environ.get("COLS", "80"))


import codecs
_DEC = codecs.getincrementaldecoder("utf-8")("replace")


def pump(fd, screen, stream, seconds):
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
            # Incremental decode: a multibyte glyph (─ = 3 bytes) can be
            # split across os.read boundaries; a per-chunk decode would
            # emit U+FFFD and desync pyte's columns (a harness artifact,
            # not a widget bug).
            stream.feed(_DEC.decode(chunk))


def make_home():
    home = tempfile.mkdtemp(prefix="nerv-short-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        # %~ expands to the (long) tempdir path, anchoring the box far
        # right — the exact condition that exposed the autowrap tear.
        f.write("PS1='nerv-test %~ %# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
    env = dict(os.environ)
    env.update(HOME=home, ZDOTDIR=zdot,
               NERV_SPECS_DIR=os.path.join(REPO, "crates/nerv-engine/tests/fixtures/specs"),
               TERM="xterm-256color")
    return home, env


def detect_tear(screen):
    glyphs = set("│╭╰├┤╮╯")
    cols = sorted({x for line in screen.display
                   for x, ch in enumerate(line) if ch in glyphs})
    far_left = [c for c in cols if c < 6]
    return cols, far_left


def run_once(rows, cols, env, home, verbose):
    screen = pyte.Screen(cols, rows)
    stream = pyte.Stream(screen)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    proc = subprocess.Popen(["/bin/zsh"], preexec_fn=os.setsid, cwd=home,
                            stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
    os.close(slave)
    try:
        pump(master, screen, stream, 2.0)
        os.write(master, b"brew ")   # 20 subcommands -> tall list
        pump(master, screen, stream, 2.0)
        border_cols, far_left = detect_tear(screen)
        if verbose:
            print(f"=== SCREEN {rows}x{cols} ===")
            for y, line in enumerate(screen.display):
                print(f"{y:2} |{line.rstrip()}")
            print("=== END ===")
        torn = bool(far_left)
        print(f"ROWS={rows:<3} border_cols={border_cols} far_left={far_left} -> {'TORN' if torn else 'OK'}")
        os.write(master, b"\x03exit\n")
        time.sleep(0.3)
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
    finally:
        os.close(master)
    return torn


def main():
    home, env = make_home()
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)
    rc = 0
    try:
        if "ROWS" in os.environ:
            torn = run_once(ROWS, COLS, env, home, verbose=True)
            rc = 1 if torn else 0
        else:
            for r in (11, 12, 13, 14, 16, 20, 24):
                if run_once(r, COLS, env, home, verbose=False):
                    rc = 1
            print("PASS" if rc == 0 else "FAIL — popup torn in a short window")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
    return rc


if __name__ == "__main__":
    sys.exit(main())
