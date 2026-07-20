#!/usr/bin/env python3
"""E2E smoke for filenames containing spaces (_nerv.zsh).

macOS is full of them — `Application Support`, `Google Drive`, iCloud
folders — so a completion that inserts them unquoted produces a line the
shell splits into two words. `cd My Folder/` is not a cd into "My
Folder"; zsh reads it as `cd <old> <new>` (the string-substitution form)
and lands somewhere else entirely, or errors.

`cd My` + Enter → cwd must be `<probe>/My Folder`. Directory completion
inserts AND runs on one Enter (the dotnav contract), so the resulting
cwd is the assertion: it proves the inserted text was shell-safe. A
screen scrape would have passed against the broken version — the line
*looked* right while the cd silently did nothing.

Run from repo root:  python3 scripts/e2e-zle-spaces.py
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
CD_SPEC = '{ "name": "cd", "description": "Change directory", "args": [{ "name": "dir", "template": "folders" }] }'


def log(msg):
    print(f"[e2e-spaces] {msg}", flush=True)


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
    """Type `keys` (bytes, or a list of chunks each given time to
    settle), press Enter once, then print $PWD."""
    master, proc = new_shell(env)
    pump(master, 2.0)
    for chunk in (keys if isinstance(keys, list) else [keys]):
        os.write(master, chunk)
        pump(master, 1.5)
    os.write(master, b"\r")
    pump(master, 1.0)
    os.write(master, b'print -r -- "CWDMARK=[$PWD]"\r')
    out = pump(master, 1.5)
    kill(master, proc)
    marks = re.findall(r"CWDMARK=\[([^\]]*)\]", out.decode(errors="replace"))
    return (marks[-1] if marks else ""), out


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-spaces-")
    probe = os.path.join(home, "probe")
    spacey = os.path.join(probe, "My Folder")
    os.makedirs(os.path.join(spacey, "deeper"), exist_ok=True)
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
        # Case 1 — accept a spacey directory and run it.
        cwd, out = cwd_after(env, b"cd My")
        ok1 = cwd.rstrip("/").endswith("My Folder")
        log(f"case1 accept    `cd My` +Enter -> cwd={cwd!r} -> {'OK' if ok1 else 'FAIL'}")

        # Case 2 — DRILL THROUGH the space. The first Tab leaves
        # `cd My\ Folder/` in the buffer (our own `(q)` quoting), so the
        # second Tab feeds that escaped text back to the engine. This is
        # the only case that proves widget-emit and engine-parse agree;
        # a unit test on either side alone cannot.
        cwd2, out2 = cwd_after(env, [b"cd My", b"\t", b"deep"])
        ok2 = cwd2.rstrip("/").endswith(os.path.join("My Folder", "deeper"))
        log(f"case2 drill     `cd My`+Tab+`deep`+Enter -> cwd={cwd2!r} -> {'OK' if ok2 else 'FAIL'}")

        if ok1 and ok2:
            log("PASS — spacey paths both accept and drill through")
            rc = 0
        else:
            log("FAIL — spacey path handling is broken")
            if not ok1:
                log(f"  case1 expected cwd ending 'My Folder', got {cwd!r}")
                log(f"  case1 tail: {out[-400:]!r}")
            if not ok2:
                log(f"  case2 expected cwd ending 'My Folder/deeper', got {cwd2!r}")
                log(f"  case2 tail: {out2[-400:]!r}")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
