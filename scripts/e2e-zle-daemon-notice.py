#!/usr/bin/env python3
"""E2E: the widget says when the daemon is down and when it is back (_nerv.zsh).

The E1 hint was a `zle -R` status line, which zle wipes as soon as the
widget returns, and it was latched once per shell: a daemon that died
later failed in silence, and its return was never mentioned.

1. No daemon: after a completion key the hint is on screen, and stays
   while typing goes on.
2. Daemon started: the next key says it is back when it has no rows to
   show (a popup is its own sign, and its paint clears the lines under
   it); the key after that takes the line away and the popup works.
3. Daemon stopped again within a minute: no second hint (backoff).

Run from repo root:  python3 scripts/e2e-zle-daemon-notice.py
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
DOWN = "[nerv] daemon not running — run: nerv start"
BACK = "[nerv] daemon is back"


def log(msg):
    print(f"[e2e-daemon-notice] {msg}", flush=True)


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

    def send(self, data, settle=1.2):
        os.write(self.master, data)
        self.pump(settle)

    def text(self):
        return "\n".join(line.rstrip() for line in self.screen.display)

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

    home = tempfile.mkdtemp(prefix="nerv-notice-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = SPECS
    env["NERV_AUTOSTART"] = "0"  # the test owns the daemon
    env["TERM"] = "xterm-256color"

    checks = []
    sh = Shell(env)
    try:
        sh.send(b"git ")
        checks.append(("1 down: the hint is on screen", DOWN in sh.text()))
        sh.send(b"c")
        checks.append(("1 down: it stays while typing", DOWN in sh.text()))

        subprocess.run([NERV, "start"], env=env, capture_output=True)
        time.sleep(1.0)
        sh.send(b"q")  # `git cq`: answered, nothing to offer
        back = sh.text()
        checks.append(("2 up: says it is back", BACK in back and DOWN not in back))
        sh.send(b"\x15")  # start the line over: `git c` has rows
        sh.send(b"git c")
        after = sh.text()
        checks.append(("2 up: the line leaves, the popup works", BACK not in after and "checkout" in after))

        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        time.sleep(0.5)
        sh.send(b"h")
        checks.append(("3 down again within a minute: no second hint", DOWN not in sh.text()))
        if not all(ok for _, ok in checks):
            log("screen:\n" + sh.text())
    finally:
        sh.close()
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        shutil.rmtree(home, ignore_errors=True)

    for name, ok in checks:
        log(f"{'OK  ' if ok else 'FAIL'} {name}")
    if len(checks) == 5 and all(ok for _, ok in checks):
        log("PASS — daemon down and back are both said, once")
        return 0
    log("FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
