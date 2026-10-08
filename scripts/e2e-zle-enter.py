#!/usr/bin/env python3
"""E2E smoke for Enter-at-segment-boundary (_nerv.zsh).

When the popup is open and the cursor sits at a completed segment
boundary (LBUFFER ends in space / `/`) and the user has NOT navigated,
Enter must run the line as typed — not inject the auto-highlighted top
row. Verifies `git ` + Enter executes bare `git` (which prints its usage
banner) rather than completing to `git checkout` and waiting.

Also verifies the opt-in path: `git ` + Tab (navigate) + Enter DOES
insert the selection (`git checkout`).

Run from repo root:  python3 scripts/e2e-zle-enter.py
Requires: cargo-built debug binaries, zsh + git on PATH.
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
    print(f"[e2e-enter] {msg}", flush=True)


def strip_ansi(raw):
    """Drop CSI sequences + save/restore so text assertions see plain bytes."""
    return re.sub(rb"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[78]", b"", raw)


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

    home = tempfile.mkdtemp(prefix="nerv-enter-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot, exist_ok=True)
    # A real directory for the shell to (not) move into below.
    os.makedirs(os.path.join(home, "work", "inner"), exist_ok=True)

    # Directory cases below need a `cd` spec with STATIC rows (the shared
    # fixtures have no `cd` spec, and filepath rows are timing- and
    # filter-sensitive — static prefix rows are not).
    import shutil as _shutil

    specdir = os.path.join(home, "specs")
    _shutil.copytree(SPECS, specdir)
    with open(os.path.join(specdir, "cd.json"), "w") as f:
        f.write(
            '{"name": "cd", "description": "Change directory", '
            '"subcommands": ['
            '{"name": "work/", "description": "work dir"}, '
            '{"name": "other", "description": "other dir"}]}'
        )
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write("PS1='%# '\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    env["NERV_SPECS_DIR"] = specdir
    env["TERM"] = "xterm-256color"

    log("starting nervd")
    subprocess.run([NERV, "start"], env=env, capture_output=True)
    time.sleep(1.0)

    rc = 1
    try:
        # --- Case 1: `git ` + Enter → executes bare git (usage banner) ---
        master, proc = new_shell(env)
        pump(master, 2.0)  # reach prompt
        os.write(master, b"git ")
        pump(master, 1.5)  # popup opens (checkout highlighted)
        os.write(master, b"\r")  # Enter — no navigation
        out1 = pump(master, 2.0)
        kill(master, proc)
        text1 = out1.decode(errors="replace")
        # Bare `git` prints its usage banner; `git checkout` (if wrongly
        # inserted) would not execute and show no usage text.
        exec_bare = "usage: git" in text1 or "These are common Git commands" in text1
        inserted_checkout = "checkout" in text1 and not exec_bare
        log(f"case1 exec_bare_git={exec_bare} inserted_checkout={inserted_checkout}")

        # --- Case 2: `git ` + Tab + Enter → inserts `git checkout` ---
        master, proc = new_shell(env)
        pump(master, 2.0)
        os.write(master, b"git ")
        pump(master, 1.5)
        os.write(master, b"\t")  # navigate → selection committed intent
        pump(master, 1.0)
        os.write(master, b"\r")  # Enter now inserts the selection
        out2 = pump(master, 1.5)
        kill(master, proc)
        text2 = out2.decode(errors="replace")
        # Tab cycles to the next row, so the inserted token is whichever
        # subcommand is selected (checkout/commit/status). Success = a
        # subcommand was inserted onto the line AND bare git was NOT
        # executed (no usage banner).
        subs = ("checkout", "commit", "status")
        nav_inserted = any(f"git {s}" in text2 for s in subs) and "usage: git" not in text2
        log(f"case2 nav_inserted_subcommand={nav_inserted}")

        # --- Case 3: partial token highlights the first item, not the
        # sentinel. `git c` → popup selects item 1 → footer `[1/N]`
        # (an item), never a bare `[N]` (the sentinel).
        master, proc = new_shell(env)
        pump(master, 2.0)
        # Type `git ` first and DISCARD its frame — that boundary state
        # legitimately shows the sentinel. Then type `c` and capture only
        # that frame, so the assertions see the final `git c` popup.
        os.write(master, b"git ")
        pump(master, 1.2)
        os.write(master, b"c")
        out3 = pump(master, 1.5)
        kill(master, proc)
        # The `›` marker sits only on a selected ITEM row (the sentinel row
        # has none), and the counter shows only when the list overflows —
        # so the marker, not `[1/N]`, is what proves an item is selected.
        item_footer = re.findall(rb"\[(\d+)/(\d+)\]", out3)
        partial_selects_item = "›".encode() in out3 and (
            not item_footer or item_footer[-1][0] == b"1"
        )
        # A filtered (partial) popup must NOT carry the sentinel row.
        no_sentinel_on_partial = b"Immediately execute" not in out3
        log(
            f"case3 partial_selects_item={partial_selects_item} "
            f"no_sentinel={no_sentinel_on_partial} footer={item_footer[-1:]}"
        )

        # --- Case 4: Tab on a partial-token popup INSERTS the highlighted
        # item (no navigation needed) — `git c` + Tab → `git checkout`.
        master, proc = new_shell(env)
        pump(master, 2.0)
        os.write(master, b"git ")
        pump(master, 1.2)
        os.write(master, b"c")
        pump(master, 1.2)
        os.write(master, b"\t")  # Tab accepts the highlighted item
        out4 = pump(master, 1.5)
        kill(master, proc)
        # Tab-chaining opens the next-level popup right after the insert,
        # so the buffer repaint interleaves SGR per character and paints
        # only the not-yet-echoed remainder over the grey ghost
        # ("heckout" after the typed "git c") — a contiguous raw
        # b"git checkout" no longer appears. Match the ANSI-stripped
        # repaint instead; either form proves the insert landed.
        plain4 = strip_ansi(out4)
        tab_inserted = b"git checkout" in plain4 or b"heckout" in plain4
        log(f"case4 tab_inserted_checkout={tab_inserted}")

        # --- Case 5: highlighted directory + Enter INSERTS without
        # running. `cd w` highlights the static `work/` row; Enter must
        # land it on the line and stay there — the shell must NOT cd.
        master, proc = new_shell(env)
        pump(master, 2.0)
        os.write(master, b"cd " + home.encode() + b"\n")
        pump(master, 1.0)  # shell cwd is now the temp HOME
        os.write(master, b"cd w")
        out5a = pump(master, 1.5)  # popup: `work/` highlighted (item 1)
        rows_shown = b"work/" in strip_ansi(out5a)
        os.write(master, b"\r")  # Enter — must insert only
        pump(master, 1.0)
        # Discard the line, then ask where we are. Ctrl-U (unix-line-
        # discard), not Ctrl-C: a bare ETX never reaches the line editor
        # reliably over this pty (probe3), while ^U always does.
        os.write(master, b"\x15")  # discard the line, then ask where we are
        pump(master, 0.5)
        os.write(master, b"pwd\n")
        out5 = pump(master, 1.5)
        kill(master, proc)
        plain5 = strip_ansi(out5)
        stayed_home = home.encode() in plain5 and b"work" not in plain5.split(home.encode())[-1][:40]
        log(f"case5 rows_shown={rows_shown} stayed_home={stayed_home}")

        # --- Case 6: the sentinel row labels what Enter will do. At a
        # segment boundary (`cd `) it reads `Enter: run`; after Tab moves
        # the highlight onto an item (sentinel still drawn, dimmed) it
        # reads `Enter: insert`. (A filtered partial hides the sentinel
        # entirely, so navigation — not typing — is the observable
        # `insert` state.)
        master, proc = new_shell(env)
        pump(master, 2.0)
        os.write(master, b"cd ")
        out6a = pump(master, 1.5)  # boundary popup, sentinel default
        os.write(master, b"\t")  # Tab: cycle sentinel → first item
        out6b = pump(master, 1.5)  # same box, item 1 highlighted
        kill(master, proc)
        label_run = b"Enter: run" in strip_ansi(out6a)
        label_insert = b"Enter: insert" in strip_ansi(out6b)
        log(f"case6 label_run={label_run} label_insert={label_insert}")

        if (
            exec_bare
            and not inserted_checkout
            and nav_inserted
            and partial_selects_item
            and no_sentinel_on_partial
            and tab_inserted
            and rows_shown
            and stayed_home
            and label_run
            and label_insert
        ):
            log(
                "PASS — sentinel executes; partial hides sentinel + selects "
                "item 1; Tab inserts the highlighted item; directory Enter "
                "inserts without running; sentinel labels Enter: run/insert"
            )
            rc = 0
        else:
            log("FAIL")
            log(f"  case1 tail: {out1[-300:]!r}")
            log(f"  case2 tail: {out2[-300:]!r}")
            log(f"  case3 tail: {out3[-300:]!r}")
            log(f"  case4 tail: {out4[-300:]!r}")
            log(f"  case5a tail: {out5a[-300:]!r}")
            log(f"  case5 tail: {out5[-300:]!r}")
            log(f"  case6a tail: {out6a[-300:]!r}")
            log(f"  case6b tail: {out6b[-300:]!r}")
    finally:
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
