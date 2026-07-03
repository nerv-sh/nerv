#!/usr/bin/env python3
"""E2E smoke for the history-based inline ghost (_nerv.zsh).

nerv should show the most recent matching history command as a grey
inline suggestion (zsh-autosuggestions / Fig style), even for a bare
command name with no spec completion. Seeds history with `pwd pbcopy`,
types `pwd`, and asserts the ghost remainder `pbcopy` is painted inline.

Run from repo root:  python3 scripts/e2e-zle-history.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

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

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")


def log(msg):
    print(f"[e2e-history] {msg}", flush=True)


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


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-hist-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    # Seed a history file the interactive shell will load on startup.
    histfile = os.path.join(home, ".zsh_history")
    with open(histfile, "w") as f:
        f.write("cd /tmp\n")
        f.write("pwd pbcopy\n")
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write(f"HISTFILE={histfile}\n")
        f.write("HISTSIZE=1000\nSAVEHIST=1000\n")
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

        pump(master, 2.0)  # reach prompt (history loaded)

        # Type a bare command name (no space): no spec completion, but the
        # history ghost should recall `pwd pbcopy`.
        os.write(master, b"pwd")
        out = pump(master, 1.5)
        text = out.decode(errors="replace")
        # The ghost remainder (" pbcopy") is painted inline via POSTDISPLAY.
        ghost_shown = "pbcopy" in text
        log(f"after 'pwd': ghost 'pbcopy' shown={ghost_shown}")

        if ghost_shown:
            log("PASS — history inline ghost recalled the previous command")
            rc = 0
        else:
            log("FAIL — no history ghost")
            log(f"  tail: {out[-300:]!r}")

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
