#!/usr/bin/env python3
"""E2E smoke for directory Enter-executes (_nerv.zsh).

Model: Enter runs, Tab drills. When the popup highlights a directory
completion (insertion ends in `/` — a dotnav pin `../` or a real folder
`cli/`), picking it with Enter must INSERT it AND run the line in a
single keypress — not require a second Enter. Descending further into
subdirectories is Tab's job.

Case 1: `cd ..` + one Enter → cwd is the parent (dotnav pin).
Case 2: `cd zz` + one Enter → cwd is `<probe>/zzdeep` (real subdir).

Both prove one Enter both completes the directory and executes; a
marker prints the resulting cwd, which is only reachable if the line
actually ran (a mere insert would swallow the marker text).

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
# Minimal `cd` spec (a folders-template positional) so `cd <partial>`
# lists real subdirectories. The vendored fixture set has no `cd` spec;
# a self-contained one keeps this test independent of the installed cache.
CD_SPEC = '{ "name": "cd", "description": "Change directory", "args": [{ "name": "dir", "template": "folders" }] }'


def log(msg):
    print(f"[e2e-dirnav] {msg}", flush=True)


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


def cwd_after(env, keys):
    """Open a shell (already cd'd into <probe>), send `keys`, then one
    Enter, then a marker that prints $PWD. Return the printed cwd."""
    master, proc = new_shell(env)
    pump(master, 2.0)  # reach prompt
    os.write(master, keys)
    pump(master, 1.5)  # popup opens, directory highlighted
    os.write(master, b"\r")  # single Enter — must insert dir AND run
    pump(master, 1.0)
    os.write(master, b'print -r -- "CWDMARK=$PWD"\r')
    out = pump(master, 1.5)
    kill(master, proc)
    marks = re.findall(r"CWDMARK=(\S+)", out.decode(errors="replace"))
    return (marks[-1] if marks else ""), out


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-dirnav-")
    probe = os.path.join(home, "probe")
    deep = os.path.join(probe, "zzdeep")  # unique prefix so `cd zz` is unambiguous
    os.makedirs(deep, exist_ok=True)
    specs = os.path.join(home, "specs")
    os.makedirs(specs, exist_ok=True)
    with open(os.path.join(specs, "cd.json"), "w") as f:
        f.write(CD_SPEC)
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')
        f.write(f'cd "{probe}"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = specs
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    rc = 1
    try:
        # Case 1: dotnav `cd ..` → parent (basename == home's basename).
        cwd1, out1 = cwd_after(env, b"cd ..")
        c1 = bool(cwd1) and cwd1.rstrip("/").endswith(os.path.basename(home))
        log(f"case1 dotnav cwd={cwd1!r} -> {'OK' if c1 else 'FAIL'}")

        # Case 2: real subdir `cd zz` → <probe>/zzdeep.
        cwd2, out2 = cwd_after(env, b"cd zz")
        c2 = bool(cwd2) and cwd2.rstrip("/").endswith("zzdeep")
        log(f"case2 subdir cwd={cwd2!r} -> {'OK' if c2 else 'FAIL'}")

        if c1 and c2:
            log("PASS — dir completion + one Enter completes AND executes")
            rc = 0
        else:
            log("FAIL — directory Enter did not execute on one keypress")
            log(f"  case1 tail: {out1[-300:]!r}")
            log(f"  case2 tail: {out2[-300:]!r}")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
