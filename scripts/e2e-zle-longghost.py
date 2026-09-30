#!/usr/bin/env python3
"""E2E regression: the ZLE popup under a ghost that wraps (_nerv.zsh).

The box opens on the line under the cursor and covers the lines of a
history ghost that wraps (as Fig's window did). zsh redraws ghost lines
that change after nerv painted — zsh-autosuggestions clears its ghost while
nerv's widget runs, draws it after, and in async mode draws it later still
— and each redraw wiped the box on them: `aws c` showed a box with no top
border until the ghost got short.

The shell loads the real zsh-autosuggestions when one is installed (the
case that broke), else nerv's own ghost. History holds one long
`git checkout <200 x>`; typing `git c` must show the whole box — top and
bottom border, both rows — starting on the line under the prompt.

Run from repo root:  python3 scripts/e2e-zle-longghost.py
Requires: cargo-built debug binaries, zsh, pyte.
"""

import codecs
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

try:
    import pyte
except ImportError:
    print("SKIP — pyte not installed (pip install pyte)")
    sys.exit(0)

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
COLS, ROWS = 80, 30
LONG = "git checkout " + "x" * 200
AUTOSUGGEST = next((p for p in [
    os.environ.get("NERV_E2E_AUTOSUGGEST", ""),
    os.path.expanduser("~/.oh-my-zsh/custom/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh"),
    "/opt/homebrew/share/zsh-autosuggestions/zsh-autosuggestions.zsh",
    "/usr/share/zsh-autosuggestions/zsh-autosuggestions.zsh",
] if p and os.path.exists(p)), None)


def log(msg):
    print(f"[e2e-longghost] {msg}", flush=True)


def pump(fd, stream, seconds, dec):
    deadline = time.time() + seconds
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.1)
        if not r:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        stream.feed(dec.decode(chunk))


def run(with_plugin, ranked=False):
    home = tempfile.mkdtemp(prefix="nerv-lg-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot)
    with open(os.path.join(home, "hist"), "w") as f:
        f.write(LONG + "\n")
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%% '\n")
        f.write(f"HISTFILE={home}/hist; HISTSIZE=100; SAVEHIST=100\n")
        if with_plugin:
            f.write(f"source {AUTOSUGGEST}\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
    env = dict(os.environ)
    # ranked: the daemon's own history ranks the ghost (the usual case);
    # otherwise the ghost comes from zsh's `$history`.
    hist = "-"
    if ranked:
        hist = os.path.join(home, "history.tsv")
        with open(hist, "w") as f:
            for _ in range(3):
                f.write(f"{int(time.time())}\t0\t{home}\t\t{LONG}\t\n")
    env.update(HOME=home, ZDOTDIR=zdot, NERV_SPECS_DIR=SPECS, TERM="xterm-256color",
               NERV_FRECENCY_FILE="-", NERV_HISTORY_FILE=hist, NERV_MISSES_FILE="-")
    env.pop("NERV_POPUP_THEME", None)

    dec = codecs.getincrementaldecoder("utf-8")("replace")
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    proc = subprocess.Popen(["/bin/zsh"], preexec_fn=os.setsid, cwd=home, stdin=slave,
                            stdout=slave, stderr=slave, env=env, close_fds=True)
    os.close(slave)
    try:
        pump(master, stream, 2.5, dec)
        os.write(master, b"clear\n")
        pump(master, stream, 1.0, dec)
        for ch in b"git c":
            os.write(master, bytes([ch]))
            pump(master, stream, 0.4, dec)
        pump(master, stream, 1.5, dec)
        lines = [l.rstrip() for l in screen.display]
    finally:
        try:
            os.write(master, b"\x03exit\n")
        except OSError:
            pass
        time.sleep(0.2)
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
        os.close(master)
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    prompt = [y for y, l in enumerate(lines) if l.startswith("% git c")]
    ghost = [y for y, l in enumerate(lines) if "xxxxx" in l]
    top = [y for y, l in enumerate(lines) if "╭" in l]
    bottom = [y for y, l in enumerate(lines) if "╰" in l]
    rows = [l for l in lines if "checkout" in l and "│" in l] + \
           [l for l in lines if "commit" in l and "│" in l]
    # No escape may reach the screen as text (a quoting slip once printed
    # `\e[4G` above the box), and the top border starts where the rows do.
    literal = any("\\e[" in l for l in lines)
    aligned = bool(top) and bool(rows) and lines[top[0]].index("╭") == rows[0].index("│")
    ok = (len(prompt) == 1 and len(ghost) >= 2 and len(top) == 1 and len(bottom) == 1
          and top[0] == prompt[0] + 1 and len(rows) == 2 and not literal and aligned)
    tag = ("zsh-autosuggestions" if with_plugin else "nerv ghost") + \
        (" + ranked history" if ranked else "")
    log(f"{tag}: prompt={prompt} ghost lines={ghost} top={top} bottom={bottom} rows={len(rows)} "
        f"literal_escape={literal} aligned={aligned} "
        f"-> {'ok' if ok else 'FAIL'}")
    if not ok:
        for y, l in enumerate(lines[:16]):
            log(f"  {y:2}|{l}")
    return ok


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli -p nerv-daemon`")
        return 2
    results = [run(False), run(False, ranked=True)]
    if AUTOSUGGEST:
        results += [run(True), run(True, ranked=True)]
    else:
        log("zsh-autosuggestions not found — plugin case skipped")
    if all(results):
        log("PASS")
        return 0
    log("FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
