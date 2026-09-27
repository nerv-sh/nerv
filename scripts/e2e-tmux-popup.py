#!/usr/bin/env python3
"""E2E: the completion popup inside tmux (docs/terminal-compat.md §6).

Runs a headless tmux server on a private socket, starts nerv-enabled zsh
panes in it, types `git c`, and reads the result back with
`tmux capture-pane -p`. tmux is the terminal under test, so its own grid
is the screen we assert on: no VT parser of our own.

  --path zle   the default ZLE widget (`nerv init zsh`)
  --path pty   the opt-in PTY path (`nerv-pty -- zsh` + _nerv-pty.zsh)

Scenarios: popup renders · a split shows it only in the active pane ·
the last-row prompt still gets the whole box · a detach and reattach
leaves it working.

Run from the repo root:  python3 scripts/e2e-tmux-popup.py --path zle
Requires: cargo-built debug binaries, tmux and zsh on PATH (measured on
tmux 3.7c).
Nothing touches the user's tmux server or $HOME: the server lives on its
own socket and every shell gets a temporary HOME/ZDOTDIR.
"""

import argparse
import fcntl
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEBUG = os.path.join(REPO, "target", "debug")
NERV = os.path.join(DEBUG, "nerv")
NERV_PTY = os.path.join(DEBUG, "nerv-pty")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
PTY_ZSH = os.path.join(REPO, "shell-integrations", "zsh", "_nerv-pty.zsh")

SOCK = f"nerv-e2e-{os.getpid()}"
SESSION = "e2e"
COLS, ROWS = 100, 20

TYPED = "git c"                # fixture git spec: checkout, commit
ITEMS = 2
BOX_ROWS = ITEMS + 4           # top border, items, separator, footer, bottom
FOOTER = re.compile(r"\[\d+/\d+\]")
BOX = set("╭╮╰╯│├┤")


def log(msg):
    print(f"[e2e-tmux] {msg}", flush=True)


def tmux(*args, check=True):
    r = subprocess.run(["tmux", "-L", SOCK, *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"tmux {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def capture(pane):
    return tmux("capture-pane", "-p", "-t", pane).splitlines()


def box_rows(lines):
    return [l for l in lines if any(ch in BOX for ch in l)]


def popup_up(lines):
    """The whole box is on screen: every row, and the [i/n] footer."""
    rows = box_rows(lines)
    return len(rows) == BOX_ROWS and any(FOOTER.search(l) for l in rows)


def wait_for(pane, pred, timeout=6.0):
    """Poll the pane until `pred(lines)` holds; return the last capture."""
    deadline = time.time() + timeout
    lines = capture(pane)
    while not pred(lines) and time.time() < deadline:
        time.sleep(0.1)
        lines = capture(pane)
    return pred(lines), lines


def dump(lines):
    for i, l in enumerate(lines):
        log(f"  {i:2d}|{l.rstrip()}")


def make_env(path):
    home = tempfile.mkdtemp(prefix="nerv-tmux-")
    zdot = os.path.join(home, "zdot")
    os.makedirs(zdot)
    prompt = "> "
    with open(os.path.join(zdot, ".zshrc"), "w") as f:
        f.write(f"PS1='{prompt}'\n")
        if path == "zle":
            f.write(f'eval "$({NERV} init zsh)"\n')
        else:
            f.write(f"source {PTY_ZSH}\n")
    env = dict(os.environ)
    env.pop("TMUX", None)          # we may be running inside the user's tmux
    env.pop("NERV_PTY_SESSION_ID", None)
    env.update(HOME=home, ZDOTDIR=zdot, SHELL="/bin/zsh",
               PATH=f"{DEBUG}:{env.get('PATH', '')}",
               NERV_SPECS_DIR=SPECS, NERV_PATH_SCAN="0", NERV_AUTOSTART="0")
    return home, env, prompt


def shell_cmd(path):
    return "/bin/zsh -i" if path == "zle" else f"{NERV_PTY} -- /bin/zsh"


class Harness:
    def __init__(self, path):
        self.path = path
        self.home, self.env, self.prompt = make_env(path)

    def start(self):
        subprocess.run([NERV, "start"], env=self.env, capture_output=True)
        # -f /dev/null keeps the user's ~/.tmux.conf (base-index, hooks) out.
        subprocess.run(["tmux", "-L", SOCK, "-f", "/dev/null", "new-session", "-d",
                        "-s", SESSION, "-x", str(COLS), "-y", str(ROWS), "sleep 3600"],
                       env=self.env, check=True)
        tmux("set", "-g", "default-size", f"{COLS}x{ROWS}")
        tmux("set", "-g", "status", "off")
        # An attached client would otherwise resize the window to its own
        # pty, which is a SIGWINCH the detach/attach case must not send.
        tmux("set", "-g", "window-size", "manual")

    def stop(self):
        tmux("kill-server", check=False)
        subprocess.run([NERV, "stop"], env=self.env, capture_output=True)
        shutil.rmtree(self.home, ignore_errors=True)

    def window(self):
        """A fresh window running the shell under test; returns its pane id."""
        pane = tmux("new-window", "-t", SESSION, "-P", "-F", "#{pane_id}",
                    shell_cmd(self.path)).strip()
        self.ready(pane)
        return pane

    def split(self, pane):
        new = tmux("split-window", "-h", "-t", pane, "-P", "-F", "#{pane_id}",
                   shell_cmd(self.path)).strip()
        self.ready(new)
        return new

    def ready(self, pane):
        # capture-pane drops trailing blanks, so a bare prompt reads ">".
        mark = self.prompt.rstrip()
        ok, lines = wait_for(pane, lambda ls: any(l.startswith(mark) for l in ls),
                             timeout=10)
        if not ok:
            dump(lines)
            raise RuntimeError(f"no prompt in {pane}")

    def type(self, pane, text):
        tmux("send-keys", "-t", pane, "-l", text)

    def key(self, pane, *keys):
        tmux("send-keys", "-t", pane, *keys)


def check_render(h):
    pane = h.window()
    h.type(pane, TYPED)
    ok, lines = wait_for(pane, popup_up)
    prompt_ok = any(l.startswith(h.prompt + TYPED) for l in lines)
    if not (ok and prompt_ok):
        dump(lines)
    return ok and prompt_ok


def check_split(h):
    a = h.window()
    b = h.split(a)
    h.key(a, "C-l")
    h.key(b, "C-l")
    h.type(a, TYPED)
    ok, lines_a = wait_for(a, popup_up)
    tmux("select-pane", "-t", b)
    # An absence can't be polled for; give B the time a stray paint would take.
    time.sleep(0.5)
    lines_b = capture(b)
    clean = not box_rows(lines_b)
    if not (ok and clean):
        log("  pane A:")
        dump(lines_a)
        log("  pane B:")
        dump(lines_b)
    return ok and clean


def check_bottom(h):
    pane = h.window()
    h.type(pane, "for i in {1..40}; do echo line$i; done")
    h.key(pane, "Enter")
    scrolled, lines = wait_for(pane, lambda ls: "line40" in ls and ls[-1].startswith(h.prompt.rstrip()))
    if not scrolled:
        log("  prompt never reached the last row")
        dump(lines)
        return False
    h.type(pane, TYPED)
    ok, lines = wait_for(pane, popup_up)
    prompt_ok = any(l.startswith(h.prompt + TYPED) for l in lines)
    if not (ok and prompt_ok):
        dump(lines)
    return ok and prompt_ok


def attach_client():
    """A real attached client: tmux has nothing to detach without one."""
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        os.execvp("tmux", ["tmux", "-L", SOCK, "attach", "-t", SESSION])
    return pid, fd


def reap(pid, fd):
    deadline = time.time() + 3
    while time.time() < deadline:
        if select.select([fd], [], [], 0.05)[0]:
            try:
                os.read(fd, 65536)
            except OSError:
                pass
        done, _ = os.waitpid(pid, os.WNOHANG)
        if done:
            break
    else:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    os.close(fd)


def window_size():
    return tmux("display", "-p", "-t", SESSION, "#{window_width}x#{window_height}").strip()


def clients():
    return [c for c in tmux("list-clients", "-F", "#{client_name}").splitlines() if c]


def check_detach_attach(h):
    pane = h.window()
    pid, fd = attach_client()
    deadline = time.time() + 5
    while not clients() and time.time() < deadline:
        time.sleep(0.1)
    if not clients():
        log("  client never attached")
        return False
    tmux("detach-client", "-s", SESSION)
    reap(pid, fd)
    pid, fd = attach_client()
    try:
        size = window_size()
        if size != f"{COLS}x{ROWS}":
            log(f"  attaching resized the window to {size}")
            return False
        h.type(pane, TYPED)
        ok, lines = wait_for(pane, popup_up)
        if not ok:
            dump(lines)
        return ok
    finally:
        tmux("detach-client", "-s", SESSION, check=False)
        reap(pid, fd)


SCENARIOS = [
    ("render", check_render),
    ("split", check_split),
    ("bottom", check_bottom),
    ("detach-attach", check_detach_attach),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--path", choices=("zle", "pty"), required=True)
    args = ap.parse_args()

    need = [NERV] + ([NERV_PTY] if args.path == "pty" else [])
    missing = [p for p in need if not os.path.exists(p)]
    if missing:
        log(f"missing binary: {missing} — run `cargo build -p nerv-cli -p nerv-pty`")
        return 2
    if not shutil.which("tmux"):
        log("tmux not on PATH")
        return 2

    # `finally` runs on exceptions only; a SIGTERM from `timeout` or CI
    # must still tear down the server, nervd and the temporary HOME.
    def bail(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, bail)
    signal.signal(signal.SIGHUP, bail)

    h = Harness(args.path)
    failed = []
    try:
        h.start()
        for name, fn in SCENARIOS:
            ok = fn(h)
            log(f"{args.path}/{name}: {'ok' if ok else 'FAIL'}")
            if not ok:
                failed.append(name)
    finally:
        h.stop()
    if failed:
        log(f"FAIL — {', '.join(failed)}")
        return 1
    log(f"PASS — {len(SCENARIOS)} scenarios on the {args.path} path")
    return 0


if __name__ == "__main__":
    sys.exit(main())
