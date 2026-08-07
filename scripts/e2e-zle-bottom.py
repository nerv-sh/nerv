#!/usr/bin/env python3
"""E2E: the ZLE popup must not collapse when the prompt sits near the bottom.

`zle -R "" "${plain[@]}"` lists its status strings the way a completion list is
drawn — columnated. If a reservation blank is narrow enough that two share one
screen line, zsh reserves fewer lines than the popup has rows. The raw paint
then walks down with ESC[B (CUD), which clamps at the bottom margin instead of
scrolling, so every remaining row piles onto the last screen line. What survives
is the top border plus the final bottom border: a box with nothing inside.

Reproduces only when BOTH hold, which is why it reads as intermittent:
  * the prompt is near the bottom (fewer rows below it than the popup needs)
  * the window is wide (COLUMNS >= roughly twice the box width)

Sweeps input x COLUMNS x gap-from-bottom and FAILS (exit 1) if any run paints
fewer rows than the popup built. The assertion is exact — a floor like
">= 6 rows" would call a 15-row box that only drew 8 of them green.

Run from repo root:  cargo build -p nerv-cli -p nerv-daemon && python3 scripts/e2e-zle-bottom.py
Env: VERBOSE=1 dumps every box, not just the broken ones.
Requires: cargo-built debug binaries, zsh, and pyte.

The widget is baked into the binary with include_str! (nerv-cli/src/main.rs), so
editing _nerv.zsh changes nothing here until you rebuild — this harness will
happily keep exercising the old widget and report green.
"""
import codecs, fcntl, os, pty, select, signal, struct, subprocess, sys, tempfile, termios, time

try:
    import pyte
except ImportError:
    print("SKIP — pyte not installed (pip install pyte)")
    sys.exit(0)

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
GLYPHS = set("│╭╰├┤╮╯")
ROWS = 30
IDLE = 0.5   # pty quiet-period that ends a pump early
VERBOSE = bool(os.environ.get("VERBOSE"))

# (typed, suggestions the engine returns, sentinel row present?)
#   `echo ` yields nothing, so `echo -` is the FIRST popup of the session —
#   no earlier reservation has scrolled spare rows in. That is the case that
#   collapses all the way down to two bare borders.
CASES = [
    ("brew ", 20, 1),
    ("brew inst", 1, 0),
    ("echo -", 2, 0),
]


def pump(fd, stream, seconds, dec):
    deadline = time.time() + seconds
    # `seconds` is a ceiling, not a sleep. Return once the pty has been quiet
    # for IDLE — 36 pty sessions at the full ceiling is ~3 minutes of pure
    # waiting. IDLE is far longer than any render step (the daemon's spec-load
    # sync window is 50ms), so an early return never truncates a repaint.
    last = time.time()
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.1)
        if not r:
            if time.time() - last > IDLE:
                return
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        last = time.time()
        # Incremental decode: a multibyte glyph (─ = 3 bytes) can split
        # across os.read boundaries and desync pyte's columns.
        stream.feed(dec.decode(chunk))


def make_home():
    home = tempfile.mkdtemp(prefix="nerv-bottom-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
    env = dict(os.environ)
    env.update(HOME=home, ZDOTDIR=zdot,
               NERV_SPECS_DIR=os.path.join(REPO, "crates/nerv-engine/tests/fixtures/specs"),
               TERM="xterm-256color")
    return home, env


def expected_rows(total_items, sentinel):
    """What __nerv_show_popup builds: visible + 4 chrome + sentinel."""
    max_vis = max(3, min(10, ROWS - 8))
    return min(total_items, max_vis) + 4 + sentinel


def run_once(cols, gap, env, home, typed, want):
    dec = codecs.getincrementaldecoder("utf-8")("replace")
    screen = pyte.Screen(cols, ROWS)
    stream = pyte.Stream(screen)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, cols, 0, 0))
    proc = subprocess.Popen(["/bin/zsh"], preexec_fn=os.setsid, cwd=home,
                            stdin=slave, stdout=slave, stderr=slave, env=env,
                            close_fds=True)
    os.close(slave)
    try:
        pump(master, stream, 2.0, dec)
        # Push the prompt down so exactly `gap` screen rows remain beneath it.
        os.write(master, ("clear; for i in {1..%d}; do print -- .; done\n"
                          % (ROWS - 1 - gap)).encode())
        pump(master, stream, 1.2, dec)
        os.write(master, typed.encode())
        pump(master, stream, 2.0, dec)

        box = [(y, l.rstrip()) for y, l in enumerate(screen.display)
               if any(ch in GLYPHS for ch in l)]
        collapsed = len(box) != want
        span = (box[-1][0] - box[0][0] + 1) if box else 0
        print("%-12s COLS=%3d gap=%2d  ->  %2d/%d box rows (span %2d)  [%s]"
              % (repr(typed), cols, gap, len(box), want, span,
                 "COLLAPSED" if collapsed else "ok"))
        if collapsed or VERBOSE:
            for y, line in box:
                print("      %3d|%s" % (y, line))
        os.write(master, b"\x03exit\n")
        time.sleep(0.2)
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
    finally:
        os.close(master)
    return collapsed


def main():
    home, env = make_home()
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)
    rc = 0
    try:
        for typed, total, sentinel in CASES:
            want = expected_rows(total, sentinel)
            print("--- %r  (popup builds %d rows)" % (typed, want))
            for cols in (80, 120, 160):
                for gap in (12, 6, 3, 1):
                    if run_once(cols, gap, env, home, typed, want):
                        rc = 1
            print()
        print("PASS" if rc == 0 else "FAIL — popup collapsed near the bottom of the screen")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
    return rc


if __name__ == "__main__":
    sys.exit(main())
