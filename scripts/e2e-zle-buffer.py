#!/usr/bin/env python3
"""E2E smoke for what the widget actually LEAVES IN THE BUFFER.

Every case here is a bug Fig itself shipped at some point, so they are
regressions worth owning rather than hypotheticals:

  1. common-prefix Tab duplication — `git st` + Tab must not produce
     `git ststatus` / `git ststa` [withfig/fig#1272]
  2. accept must preserve the whole line, not just the completed word —
     `git add . && git ch` + Tab keeps the `git add . && ` prefix
     [withfig/fig#45]
  3. kill-line then retype on the SAME line still completes — Fig went
     dead until Enter was pressed [withfig/fig#101]
  4. a long custom prompt (powerlevel10k-style) must not silently
     disable completion [withfig/fig#1163]

Assertions read the real `$BUFFER` through a probe widget bound to
^X^P, not a screen scrape: the popup redraws over the line, so scraped
text can look right while BUFFER holds something else.

Run from repo root:  python3 scripts/e2e-zle-buffer.py
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
GIT_SPEC = """{
  "name": "git",
  "description": "Version control",
  "subcommands": [
    { "name": "status", "description": "Show status" },
    { "name": "stash", "description": "Stash changes" },
    { "name": "checkout", "description": "Switch branches" },
    { "name": "cherry-pick", "description": "Apply a commit" },
    { "name": "add", "description": "Stage files" }
  ]
}"""

PROBE = (
    "__nerv_probe() { print -rn -- $'\\n'\"BUFMARK[$BUFFER]\"$'\\n'; }\n"
    "zle -N __nerv_probe\n"
    "bindkey '^X^P' __nerv_probe\n"
)


def log(msg):
    print(f"[e2e-buffer] {msg}", flush=True)


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


def buffer_after(env, steps):
    """Run `steps` (list of (bytes, settle_seconds)) then read BUFFER."""
    master, proc = new_shell(env)
    pump(master, 2.0)
    for keys, settle in steps:
        os.write(master, keys)
        pump(master, settle)
    os.write(master, b"\x18\x10")  # ^X^P — probe widget prints BUFFER
    out = pump(master, 1.2)
    os.write(master, b"\x15")  # ^U — clear so the shell exits clean
    kill(master, proc)
    text = out.decode(errors="replace")
    marks = re.findall(r"BUFMARK\[([^\]]*)\]", text)
    return (marks[-1] if marks else None), text


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli`")
        return 2

    home = tempfile.mkdtemp(prefix="nerv-buffer-")
    probe_dir = os.path.join(home, "probe")
    os.makedirs(probe_dir, exist_ok=True)
    specs = os.path.join(home, "specs")
    os.makedirs(specs, exist_ok=True)
    with open(os.path.join(specs, "git.json"), "w") as f:
        f.write(GIT_SPEC)
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)

    def write_rc(prompt):
        with open(os.path.join(zdot, ".zshrc"), "w") as f:
            f.write(f"PS1={prompt}\n")
            f.write(f'eval "$({NERV} init zsh)"\n')
            f.write(PROBE)
            f.write(f'cd "{probe_dir}"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = specs
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    results = []
    try:
        # 1 — common-prefix Tab must not duplicate the typed stem.
        write_rc("'%# '")
        buf, out = buffer_after(env, [(b"git st", 1.5), (b"\t", 0.8)])
        ok = buf is not None and "stst" not in buf and buf.startswith("git st")
        results.append(("common-prefix no duplication", ok, buf))

        # 2 — accepting must keep everything left of the completed word.
        buf2, out2 = buffer_after(env, [(b"git add . && git ch", 1.5), (b"\t", 0.8)])
        ok2 = buf2 is not None and buf2.startswith("git add . && git ")
        results.append(("accept preserves whole line", ok2, buf2))

        # 3 — kill-line, retype, complete on the SAME line.
        buf3, out3 = buffer_after(
            env, [(b"ls -la", 0.8), (b"\x15", 0.5), (b"git ch", 1.5), (b"\t", 0.8)]
        )
        ok3 = buf3 is not None and "checkout" in buf3
        results.append(("completes after kill-line", ok3, buf3))

        # 5 — Esc dismisses the popup and leaves the typed text alone
        # (Fig regressed to Esc doing nothing, withfig/fig#2259). The
        # popup is drawn with printf over the line, so assert on BUFFER:
        # a dismiss must not mutate or accept anything.
        buf5, out5 = buffer_after(env, [(b"git ch", 1.5), (b"\x1b", 0.8)])
        ok5 = buf5 == "git ch"
        results.append(("Esc dismisses without mutating", ok5, buf5))

        # 6 — Right-arrow at EOL accepts the inline ghost.
        buf6, out6 = buffer_after(env, [(b"git chec", 1.5), (b"\x1b[C", 0.8)])
        ok6 = buf6 is not None and buf6.startswith("git checkout")
        results.append(("Right-arrow accepts ghost", ok6, buf6))

        # 4 — a long custom prompt must not disable completion.
        write_rc("'%F{blue}~/very/long/project/path%f %F{green}git:(main)%f ❯ '")
        buf4, out4 = buffer_after(env, [(b"git ch", 1.5), (b"\t", 0.8)])
        ok4 = buf4 is not None and "checkout" in buf4
        results.append(("works under a long prompt", ok4, buf4))
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    failed = 0
    for name, ok, buf in results:
        log(f"{'OK  ' if ok else 'FAIL'} {name} -> BUFFER={buf!r}")
        if not ok:
            failed += 1

    if failed:
        log(f"FAIL — {failed}/{len(results)} buffer cases wrong")
        return 1
    log(f"PASS — {len(results)}/{len(results)} buffer cases correct")
    return 0


if __name__ == "__main__":
    sys.exit(main())
