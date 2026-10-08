#!/usr/bin/env python3
"""E2E for what completing in the middle of a line must not break (_nerv.zsh).

Case A — the loading hint stays. A spec too big to parse inside the
  daemon's sync window answers the first key with "loading"; the grey
  `…loading` line has to still be on screen once the widget has returned
  (a `zle -R` status line is wiped at that point), and give way to rows
  on a later key.
Case B — a recalled line is left alone. Up on an empty prompt brings back
  a whole command; a popup over it would take the next Up (cycling rows
  instead of older history) and Enter (inserting a row instead of running).
  Up, Up, Enter must run the command before last.
Case C — the word after the cursor survives. With the cursor parked at the
  start of `-m`, typing `--al` and accepting `--all` must not eat `-m`.

Run from repo root:  python3 scripts/e2e-zle-midline.py
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
ROWS, COLS = 40, 100


def log(msg):
    print(f"[e2e-midline] {msg}", flush=True)


class Shell:
    def __init__(self, env):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.proc = subprocess.Popen(
            ["/bin/zsh"],
            preexec_fn=os.setsid,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            close_fds=True,
        )
        os.close(slave)
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.pump(2.0)

    def pump(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            r, _, _ = select.select([self.master], [], [], 0.05)
            if self.master in r:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                self.stream.feed(chunk)

    def send(self, data, settle=1.0):
        os.write(self.master, data)
        self.pump(settle)

    def lines(self):
        return [line.rstrip() for line in self.screen.display]

    def text(self):
        return "\n".join(self.lines())

    def close(self):
        try:
            os.write(self.master, b"\x03\nexit\n")
        except OSError:
            pass
        time.sleep(0.2)
        self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        os.close(self.master)


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-midline-")
    specs = os.path.join(home, "specs")
    shutil.copytree(SPECS, specs)
    # Big enough that a debug-build parse outlasts the sync window.
    subs = ",".join(
        f'{{"name":"sub{i:06}","description":"subcommand number {i}"}}'
        for i in range(200_000)
    )
    with open(os.path.join(specs, "big.json"), "w") as f:
        f.write(f'{{"name":"big","subcommands":[{subs}]}}')
    # A command whose recalled line still has a row to offer (`beta` →
    # `betamax`), so a query on recall would open a popup.
    with open(os.path.join(specs, "say.json"), "w") as f:
        f.write(
            '{"name":"say","subcommands":[{"name":"alpha"},{"name":"alphabet"},'
            '{"name":"beta"},{"name":"betamax"}]}'
        )
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    buf_file = os.path.join(home, "buffer.txt")
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f"HISTFILE={home}/hist\nHISTSIZE=100\nSAVEHIST=100\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
        f.write(f'dump-buffer() {{ print -rn -- "$BUFFER" > {buf_file} }}\n')
        f.write("zle -N dump-buffer\nbindkey '^X^B' dump-buffer\n")
        f.write('say() { print -r -- "SAID-$1" }\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = specs
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    ok = {}
    try:
        # --- Case A
        sh = Shell(env)
        sh.send(b"big ", settle=0.6)
        hint_shown = "…loading" in sh.text()
        sh.pump(6.0)  # let the parse land
        sh.send(b"sub00000", settle=2.0)
        after = sh.text()
        rows_landed = "sub000001" in after
        hint_gone = "…loading" not in after
        ok["A loading hint stays, then yields to rows"] = (
            hint_shown and rows_landed and hint_gone
        )
        if not ok["A loading hint stays, then yields to rows"]:
            log(f"  A: hint_shown={hint_shown} rows_landed={rows_landed} hint_gone={hint_gone}")
            log("  A screen:\n" + after)
        sh.close()

        # --- Case B
        sh = Shell(env)
        sh.send(b"say alpha\r")
        sh.send(b"say beta\r")
        sh.send(b"\x1b[A")
        recalled = sh.text()
        no_popup = "╭" not in recalled
        sh.send(b"\x1b[A")
        sh.send(b"\r")
        ran = [line for line in sh.lines() if line == "SAID-alpha"]
        ok["B recalled line: no popup, Up reaches older history"] = (
            no_popup and len(ran) == 2
        )
        if not ok["B recalled line: no popup, Up reaches older history"]:
            log(f"  B: no_popup={no_popup} SAID-alpha outputs={len(ran)} (want 2)")
            log("  B screen:\n" + sh.text())
        sh.close()

        # --- Case C
        sh = Shell(env)
        sh.send(b"git commit -m x")
        sh.send(b"\x1b[D" * 4)  # cursor to the start of `-m`
        sh.send(b"--al")
        sh.send(b"\t")
        sh.send(b"\x18\x02", settle=0.5)
        try:
            with open(buf_file) as f:
                buffer = f.read()
        except OSError:
            buffer = "<no buffer dump>"
        ok["C the word after the cursor survives the insert"] = (
            buffer == "git commit --all -m x"
        )
        if buffer != "git commit --all -m x":
            log(f"  C: buffer={buffer!r} (want 'git commit --all -m x')")
        sh.close()
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        shutil.rmtree(home, ignore_errors=True)

    for name, passed in ok.items():
        log(f"{'OK  ' if passed else 'FAIL'} {name}")
    if ok and all(ok.values()) and len(ok) == 3:
        log("PASS — mid-line completion keeps the hint, the history and the next word")
        return 0
    log("FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
