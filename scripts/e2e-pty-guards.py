#!/usr/bin/env python3
"""Guard symmetry for the PTY bootstrap shims (`_nerv-pty.{zsh,bash,fish}`).

Slice 01 (PLAN-11-13-56): the three shims must agree on when to re-exec
under `nerv-pty` — only with the `NERV_PTY=1` opt-in, on an interactive
TTY shell — and must pass the shell through identically.

Uses a fake `nerv-pty` stub on PATH: if a shim execs, `STUB-EXECED`
appears; if it correctly stands down, the shell survives to print its
`SURVIVED-<shell>` marker. Each shell runs under a real pty so the
`-t 0` / `-t 1` checks see a TTY.

Cases per shell (zsh, bash, fish when installed):
  A. no NERV_PTY, no session id  → must NOT exec (SURVIVED, no STUB-EXECED)
  B. NERV_PTY=1, no session id   → MUST exec (STUB-EXECED; positive control
     so the guards cannot over-block into a silent no-op) and autostart
     `nerv start`
  C. NERV_PTY=1 + NERV_AUTOSTART=0 → MUST exec but NOT run `nerv start`
     (same opt-out ZLE `_nerv.zsh` honors)

Run from the repo root:  python3 scripts/e2e-pty-guards.py
"""

import os
import pty
import select
import shutil
import stat
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHIMS = {
    "zsh": os.path.join(REPO, "shell-integrations", "zsh", "_nerv-pty.zsh"),
    "bash": os.path.join(REPO, "shell-integrations", "bash", "_nerv-pty.bash"),
    "fish": os.path.join(REPO, "shell-integrations", "fish", "_nerv-pty.fish"),
}


def log(msg):
    print(f"[guards] {msg}", flush=True)


def drain(fd, seconds):
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


def run_case(shell_bin, shim, marker, env, **extra):
    """Source the shim under a pty; return the captured output."""
    if shell_bin.endswith("fish"):
        argv = [shell_bin, "-i", "-C", f"source {shim}", "-C", f"echo {marker}"]
    else:
        argv = [shell_bin, "-i", "-c", f"source {shim}; echo {marker}"]
    env = dict(env, **extra)
    master, slave = pty.openpty()
    try:
        proc = subprocess.Popen(
            argv, stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True,
        )
        os.close(slave)
        slave = None
        out = drain(master, 3.0)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        if slave is not None:
            os.close(slave)
    return out


def exec_shell(out):
    """Basename of the shell the shim handed to the stub (`-- <shell> ...`)."""
    for line in out.decode(errors="replace").splitlines():
        if "STUB-EXECED args:" in line:
            words = line.split("STUB-EXECED args:", 1)[1].split()
            if "--" in words and words.index("--") + 1 < len(words):
                return os.path.basename(words[words.index("--") + 1])
    return None


def started(start_log):
    """True (and resets the log) when the stub `nerv start` ran."""
    # `nerv start` is backgrounded by the shim; give it a beat to land.
    time.sleep(0.3)
    if not os.path.exists(start_log):
        return False
    os.unlink(start_log)
    return True


def main():
    for name, path in SHIMS.items():
        if not os.path.exists(path):
            log(f"missing shell integration: {path}")
            return 2

    fakebin = tempfile.mkdtemp(prefix="nerv-e2e-guards-bin-")
    with open(os.path.join(fakebin, "nerv-pty"), "w") as f:
        f.write('#!/bin/sh\necho "STUB-EXECED args: $@"\n')
    start_log = os.path.join(fakebin, "nerv-start.log")
    with open(os.path.join(fakebin, "nerv"), "w") as f:
        f.write(f'#!/bin/sh\necho "$@" >> "{start_log}"\n')
    for name in ("nerv-pty", "nerv"):
        os.chmod(os.path.join(fakebin, name), 0o755 | stat.S_IXUSR)

    home = tempfile.mkdtemp(prefix="nerv-e2e-guards-home-")
    # An (empty) zshrc keeps `zsh -i` out of its first-run installer menu.
    open(os.path.join(home, ".zshrc"), "w").close()
    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = home
    env["NERV_PTY_BIN"] = os.path.join(fakebin, "nerv-pty")
    env["PATH"] = fakebin + os.pathsep + env.get("PATH", "")
    env.pop("NERV_PTY", None)
    env.pop("NERV_PTY_SESSION_ID", None)
    env.pop("NERV_AUTOSTART", None)

    shells = {}
    for name in ("zsh", "bash", "fish"):
        found = name if os.path.exists(f"/bin/{name}") else shutil.which(name)
        if found:
            shells[name] = found
        else:
            log(f"{name} not installed — skipping")
    if not shells:
        log("no shells found — nothing to check")
        return 0

    failures = 0
    for name, shell_bin in shells.items():
        marker = f"SURVIVED-{name}".encode()
        # A: no opt-in → stand down.
        out = run_case(shell_bin, SHIMS[name], marker.decode(), env)
        survived, execed = marker in out, b"STUB-EXECED" in out
        ran_start = started(start_log)
        ok_a = survived and not execed and not ran_start
        log(f"{name} no-opt-in: SURVIVED={survived} EXECED={execed} START={ran_start} -> {ok_a}")
        if not ok_a:
            log(f"  tail repr: {out[-300:]!r}")
            failures += 1
        # B: opt-in → must hand over (guards must not over-block).
        out = run_case(shell_bin, SHIMS[name], marker.decode(), env, NERV_PTY="1")
        execed, ran_start = b"STUB-EXECED" in out, started(start_log)
        # The wrapped shell must be the one that sourced the shim, not the
        # login `$SHELL` (a zsh user running `bash` must stay in bash).
        same_shell = exec_shell(out) == name
        ok_b = execed and ran_start and same_shell
        log(
            f"{name} opt-in: EXECED={execed} START={ran_start} "
            f"SHELL={exec_shell(out)} -> {ok_b}"
        )
        if not ok_b:
            log(f"  tail repr: {out[-300:]!r}")
            failures += 1
        # C: opt-in + NERV_AUTOSTART=0 → hand over, but leave nervd alone.
        out = run_case(
            shell_bin, SHIMS[name], marker.decode(), env, NERV_PTY="1", NERV_AUTOSTART="0",
        )
        execed, ran_start = b"STUB-EXECED" in out, started(start_log)
        ok_c = execed and not ran_start
        log(f"{name} autostart-off: EXECED={execed} START={ran_start} -> {ok_c}")
        if not ok_c:
            log(f"  tail repr: {out[-300:]!r}")
            failures += 1

    if failures:
        log(f"FAIL — {failures} guard case(s) red")
        return 1
    log("PASS — all three shims agree on the opt-in/TTY guard")
    return 0


if __name__ == "__main__":
    sys.exit(main())
