#!/usr/bin/env python3
"""E2E smoke for command-name completion on the first token (_nerv.zsh).

Typing a command name is a completion like any other: `doc` offers
`docker` in the popup and Tab inserts it, while a name that is already
complete (`git`) offers nothing — the popup preselects its first row, so
a leftover row would make Enter run the wrong command. A shell alias
(`k`) counts as complete even though the daemon cannot see it. The
history ghost keeps its priority over the popup, and survives the daemon
being down, since it never needed the engine.

Run from repo root:  python3 scripts/e2e-zle-cmdname.py
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

# History seeds the ghost with a remainder no popup row can contain, so
# finding it in the output proves the ghost survived the popup paint.
GHOST_MARK = "zzzghostmark"


def strip_ansi(raw):
    """Drop CSI sequences + save/restore so text assertions see plain bytes."""
    return re.sub(rb"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[78]", b"", raw)


def log(msg):
    print(f"[e2e-cmdname] {msg}", flush=True)


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

    home = tempfile.mkdtemp(prefix="nerv-cmdname-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    emptybin = os.path.join(home, "emptybin")
    os.makedirs(emptybin, exist_ok=True)
    histfile = os.path.join(home, ".zsh_history")
    with open(histfile, "w") as f:
        f.write(f"docker {GHOST_MARK}\n")
        f.write(f"pwd {GHOST_MARK}\n")
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write(f"HISTFILE={histfile}\n")
        f.write("HISTSIZE=1000\nSAVEHIST=1000\n")
        f.write("PS1='%# '\n")
        f.write("alias k=kubectl\n")
        # An empty PATH keeps zsh's own completion from producing the
        # same word on a Tab that nerv failed to handle. `nerv` itself
        # is reached through the absolute NERV_BIN the init block sets.
        f.write(f"PATH={emptybin}\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = SPECS
    env["NERV_FRECENCY_FILE"] = "-"
    # Keep the rows to the fixture specs: a real PATH would make the
    # popup's contents differ by machine.
    env["NERV_PATH_SCAN"] = "0"
    env["NERV_MISSES_FILE"] = os.path.join(home, "misses.tsv")
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    failures = []
    master = None
    proc = None
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
        pump(master, 2.0)  # reach prompt

        # 1 + 3: a partial command name opens the popup, and the history
        # ghost is painted alongside it.
        os.write(master, b"doc")
        text = pump(master, 1.5).decode(errors="replace")
        if "docker" not in text or "[1/" not in text:
            failures.append("'doc' did not open a command-name popup")
        log(f"'doc': popup={'[1/' in text} docker={'docker' in text}")
        if GHOST_MARK not in text:
            failures.append("history ghost was lost under the popup")
        log(f"'doc': history ghost={GHOST_MARK in text}")

        # Tab takes the highlighted row: the line becomes the full name.
        # The repaint only covers the not-yet-echoed remainder, so
        # either the whole word or that tail proves the insert landed.
        os.write(master, b"\t")
        raw = pump(master, 1.5)
        plain = strip_ansi(raw)
        tab_inserted = b"docker" in plain
        # The name is now complete, so nerv closes the popup behind the
        # insert. A Tab that fell through to zsh's own completion would
        # leave the box up.
        popup_closed = "[1/" not in raw.decode(errors="replace")
        if not (tab_inserted and popup_closed):
            failures.append("Tab did not insert the command name")
        log(f"'doc'+Tab: inserted={tab_inserted} popup_closed={popup_closed}")

        # 2: a name that is already complete offers nothing. `gi` must
        # open a popup first, or the absence below would prove nothing.
        os.write(master, b"\x15")  # ctrl-u: clear the line
        pump(master, 0.6)
        os.write(master, b"gi")
        text = pump(master, 1.5).decode(errors="replace")
        if "[1/" not in text:
            failures.append("'gi' drew no popup — the 'git' check would be vacuous")
        log(f"'gi': popup={'[1/' in text}")
        os.write(master, b"t")
        text = pump(master, 1.5).decode(errors="replace")
        if "[1/" in text:
            failures.append("exact command name 'git' still drew a popup")
        log(f"'git': popup redrawn={'[1/' in text}")

        # An alias is a finished command the daemon can't see. Without a
        # widget-side guard the popup would offer `kubectl` and Enter
        # would swap the line for it.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"k")
        text = pump(master, 1.5).decode(errors="replace")
        if "[1/" in text or "kubectl" in text:
            failures.append("alias 'k' drew a command-name popup")
        log(f"alias 'k': popup={'[1/' in text}")

        # Leading whitespace (the HIST_IGNORE_SPACE habit) is the same
        # command to the engine, so the guard must strip it too.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"  k")
        text = pump(master, 1.5).decode(errors="replace")
        if "[1/" in text or "kubectl" in text:
            failures.append("space-prefixed alias '  k' drew a popup")
        log(f"alias '  k': popup={'[1/' in text}")

        # With no daemon the history ghost must still be painted — it
        # never needed the engine.
        os.write(master, b"\x15")
        pump(master, 0.6)
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        time.sleep(0.5)
        os.write(master, b"pw")
        text = pump(master, 2.0).decode(errors="replace")
        if GHOST_MARK not in text:
            failures.append("history ghost was lost when the daemon was down")
        log(f"daemon down: history ghost={GHOST_MARK in text}")

        os.write(master, b"\x15exit\n")
        time.sleep(0.3)
    except OSError as e:
        failures.append(f"pty error: {e}")
    finally:
        if proc is not None:
            proc.send_signal(signal.SIGTERM)
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
        if master is not None:
            os.close(master)
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    # 4: none of those keystrokes may be tallied as a missing spec.
    tally = env["NERV_MISSES_FILE"]
    recorded = ""
    if os.path.exists(tally):
        with open(tally) as f:
            recorded = f.read().strip()
    if recorded:
        failures.append(f"first-token keystrokes were tallied: {recorded!r}")
    log(f"misses.tsv: {recorded or '(empty)'}")

    if failures:
        for f in failures:
            log(f"FAIL — {f}")
        return 1
    log(
        "PASS — command-name popup + Tab insert, exact-name and alias "
        "silence, ghost (with and without a daemon), clean tally"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
