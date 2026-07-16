#!/usr/bin/env python3
"""E2E smoke for dotnav Enter-executes (_nerv.zsh).

When the popup highlights a dotnav pin (`./` / `../`) as a real item
(mid-token, e.g. the user typed `cd ..` with no trailing slash), Enter
must INSERT the pin AND run the line in a single keypress — not require
a second Enter. Dotnav pins are terminal navigation targets, not tokens
to drill into.

Verifies: shell starts in `<home>/probe`, user types `cd ..` + one Enter.
If the fix works the cwd is now `<home>`; a follow-up marker prints it.
If Enter only inserted (`cd ../`, unexecuted), the marker text would be
appended to the buffer instead and never run — no CWDMARK appears.

Run from repo root:  python3 scripts/e2e-zle-dotnav.py
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


def log(msg):
    print(f"[e2e-dotnav] {msg}", flush=True)


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


def new_shell(env):
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
    return master, proc


def kill(master, proc):
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


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-dotnav-")
    # `probe` is where the shell starts; its parent is `home`. `cd ..`
    # must land back in `home`, which the marker below prints.
    probe = os.path.join(home, "probe")
    os.makedirs(probe, exist_ok=True)
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
        f.write(f'cd "{probe}"\n')

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
        master, proc = new_shell(env)
        pump(master, 2.0)  # reach prompt (already cd'd into probe)
        os.write(master, b"cd ..")  # NO trailing slash → `../` is item 1
        pump(master, 1.5)  # popup opens, `../` highlighted (SELECTED=1)
        os.write(master, b"\r")  # single Enter — must insert `../` AND run
        pump(master, 1.0)
        # Marker prints the cwd. Only reachable if the previous line
        # actually executed (fresh prompt); otherwise this text lands in
        # the still-open `cd ../` buffer and never runs.
        os.write(master, b'print -r -- "CWDMARK=$PWD"\r')
        out = pump(master, 1.5)
        kill(master, proc)
        text = out.decode(errors="replace")

        marks = re.findall(r"CWDMARK=(\S+)", text)
        cwd = marks[-1] if marks else ""
        # After `cd ..` from <home>/probe the cwd is <home> — its
        # basename is the temp dir name, and it must NOT end in /probe.
        left_probe = bool(cwd) and not cwd.rstrip("/").endswith("probe")
        landed_home = bool(cwd) and cwd.rstrip("/").endswith(os.path.basename(home))
        log(f"cwd_after_one_enter={cwd!r} left_probe={left_probe} landed_home={landed_home}")

        if left_probe and landed_home:
            log("PASS — `cd ..` + one Enter executed and landed in parent")
            rc = 0
        else:
            log("FAIL — dotnav Enter did not execute on one keypress")
            log(f"  tail: {out[-300:]!r}")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
