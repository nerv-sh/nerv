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
leaves it working · (zle) a resize closes the popup and chains to the
user's own WINCH handler, in both the function and the `trap` form.

Run from the repo root:  python3 scripts/e2e-tmux-popup.py --path zle
Requires: `cargo build -p nerv-cli -p nerv-pty` first, and tmux and zsh on
PATH (measured on tmux 3.7c). Not an `--all-features` build: its
`profiling_early_exit` makes nerv-pty quit at the shell's first output,
which reads here as a pane that dies at once.
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
ZSH = shutil.which("zsh")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")
PTY_ZSH = os.path.join(REPO, "shell-integrations", "zsh", "_nerv-pty.zsh")

SOCK = f"nerv-e2e-{os.getpid()}"
SESSION = "e2e"
COLS, ROWS = 100, 20

TYPED = "git c"                # fixture git spec: checkout, commit
ITEMS = 2                      # box rows = items + 4: borders, separator, footer
FOOTER = re.compile(r"\[\d+(/\d+)?\]")    # [i/n], or [n] while the sentinel row is selected
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
    """The whole `git c` box is on screen: every row, and the [i/n] footer."""
    rows = box_rows(lines)
    return len(rows) == ITEMS + 4 and any(FOOTER.search(l) for l in rows)


def box_whole(lines, min_items):
    """A complete box of at least `min_items` rows: contiguous, top and
    bottom borders, separator and footer. Row counts differ by path (ZLE
    adds an "↩ Immediately execute" row after a trailing space)."""
    idx = [i for i, l in enumerate(lines) if any(ch in BOX for ch in l)]
    if not idx or idx != list(range(idx[0], idx[-1] + 1)):
        return False
    rows = [lines[i] for i in idx]
    return ("╭" in rows[0] and "╰" in rows[-1] and any("├" in r for r in rows)
            and any(FOOTER.search(r) for r in rows) and len(rows) - 4 >= min_items)


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
            # A user WINCH handler set before nerv loads, in either form:
            # the resize case checks that nerv chains to it.
            # The list trap ends non-zero on purpose: zsh ignores that
            # status, and chaining must not turn it into an interrupt.
            f.write('if [[ -n $E2E_LIST_TRAP ]]; then trap \': >> "$HOME/user-winch"; false\' WINCH\n')
            f.write('else TRAPWINCH() { : >> "$HOME/user-winch" }; fi\n')
            f.write(f'eval "$({NERV} init zsh)"\n')
        else:
            f.write(f"source {PTY_ZSH}\n")
    env = dict(os.environ)
    env.pop("TMUX", None)          # we may be running inside the user's tmux
    env.pop("NERV_PTY_SESSION_ID", None)
    env.update(HOME=home, ZDOTDIR=zdot, SHELL=ZSH,
               PATH=f"{DEBUG}:{env.get('PATH', '')}",
               NERV_SPECS_DIR=SPECS, NERV_PATH_SCAN="0", NERV_AUTOSTART="0")
    return home, env, prompt


def shell_cmd(path, env=""):
    return f"{env}{ZSH} -i" if path == "zle" else f"{env}{NERV_PTY} -- {ZSH}"


class Harness:
    def __init__(self, path):
        self.path = path
        self.home, self.env, self.prompt = make_env(path)

    def start(self):
        subprocess.run([NERV, "start"], env=self.env, capture_output=True)
        # `nerv start` returns before nervd serves; a first keystroke that
        # beats it gets no popup. Wait until a completion comes back.
        deadline = time.time() + 10
        while time.time() < deadline:
            r = subprocess.run([NERV, "_complete", TYPED, str(len(TYPED))],
                               env=self.env, capture_output=True, text=True)
            if "checkout" in r.stdout:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError("nervd never answered a completion")
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

    def window(self, env=""):
        """A fresh window running the shell under test; returns its pane id."""
        pane = tmux("new-window", "-t", SESSION, "-P", "-F", "#{pane_id}",
                    shell_cmd(self.path, env)).strip()
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
        # One key at a time, at typing speed. A burst (`send-keys -l` of the
        # whole string) races the widget: a Space landing while the `g`
        # popup is up accepts `gh` — a typeahead issue outside this test.
        for ch in text:
            tmux("send-keys", "-t", pane, "-l", ch)
            time.sleep(0.05)

    def key(self, pane, *keys):
        tmux("send-keys", "-t", pane, *keys)


def cursor_y(pane):
    return int(tmux("display", "-p", "-t", pane, "#{cursor_y}").strip())


def check_render(h):
    """Away from the bottom the popup draws in place: the prompt stays on
    row 0 while the popup grows (g → git → git c), no spurious scroll."""
    pane = h.window()
    h.key(pane, "C-l")
    wait_for(pane, lambda ls: ls and ls[0].startswith(h.prompt.rstrip()))
    h.type(pane, TYPED)
    ok, lines = wait_for(pane, popup_up)
    prompt_ok = bool(lines) and lines[0].startswith(h.prompt + TYPED)
    row0 = cursor_y(pane) == 0
    if not (ok and prompt_ok and row0):
        log(f"  cursor_y={cursor_y(pane)}")
        dump(lines)
    return ok and prompt_ok and row0


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


def fill_to_bottom(h, pane):
    """Scroll the prompt onto the last row; False if it never gets there."""
    tmux("send-keys", "-t", pane, "-l", "for i in {1..40}; do echo line$i; done")
    h.key(pane, "Enter")
    ok, lines = wait_for(pane, lambda ls: "line40" in ls and ls[-1].startswith(h.prompt.rstrip()))
    if not ok:
        log("  prompt never reached the last row")
        dump(lines)
    return ok


def check_bottom_grow(h):
    """On the last row, a popup that grows (`g`: gh, git → `git `: 4
    subcommands) must scroll for the extra rows, not clamp at the bottom."""
    pane = h.window()
    if not fill_to_bottom(h, pane):
        return False
    h.type(pane, "g")
    ok, lines = wait_for(pane, popup_up)
    if not ok:
        log("  `g` popup never opened")
        dump(lines)
        return False
    h.type(pane, "it ")
    ok, lines = wait_for(pane, lambda ls: box_whole(ls, 4))
    cy = cursor_y(pane)
    kept = cy < len(lines) and lines[cy].startswith(h.prompt + "git")
    if not (ok and kept):
        dump(lines)
    return ok and kept


def check_bottom(h):
    pane = h.window()
    if not fill_to_bottom(h, pane):
        return False
    h.type(pane, TYPED)
    ok, lines = wait_for(pane, popup_up)
    prompt_ok = any(l.startswith(h.prompt + TYPED) for l in lines)
    if not (ok and prompt_ok):
        dump(lines)
    return ok and prompt_ok


def check_resize(h, env=""):
    """A resize erases the popup (zsh redraws the prompt); the next key
    must not steer a popup the user can no longer see."""
    pane = h.window(env)
    h.split(pane)
    tmux("select-pane", "-t", pane)
    h.type(pane, TYPED)
    ok, lines = wait_for(pane, popup_up)
    if not ok:
        log("  popup never opened")
        dump(lines)
        return False
    marker = os.path.join(h.home, "user-winch")
    if os.path.exists(marker):          # the split above already sent one
        os.remove(marker)
    tmux("resize-pane", "-Z", "-t", pane)
    gone, lines = wait_for(pane, lambda ls: not box_rows(ls))
    if not gone:
        log("  popup survived the resize")
        dump(lines)
        return False
    # The trap runs when zsh gets the signal, not when resize-pane returns.
    deadline = time.time() + 3
    while not os.path.exists(marker) and time.time() < deadline:
        time.sleep(0.1)
    chained = os.path.exists(marker)
    h.key(pane, "Down")
    # Unzoom back to the original geometry: the popup is still gone, which
    # is why this is a trap and not a $COLUMNS:$LINES comparison.
    tmux("resize-pane", "-Z", "-t", pane)
    h.key(pane, "Down")
    # An absence can't be polled for; give a stray repaint time to land.
    time.sleep(0.8)
    lines = capture(pane)
    hidden = not box_rows(lines)
    # The cursor's row, not any row: an aborted line stays on screen with
    # a fresh prompt drawn under it.
    cy = cursor_y(pane)
    kept = cy < len(lines) and lines[cy].startswith(h.prompt + TYPED)
    if not kept:
        log("  the resize dropped the typed line")
        dump(lines)
        return False
    if not hidden:
        log("  Down repainted the popup after the resize")
        dump(lines)
    if not chained:
        log("  the user's TRAPWINCH did not run")
    # Typing again must open a fresh popup: the reset closed it, not broke it.
    h.key(pane, "BSpace")
    h.type(pane, "c")
    reopened, lines = wait_for(pane, popup_up)
    if not reopened:
        log("  typing after the resize did not reopen the popup")
        dump(lines)
    return hidden and chained and reopened


def check_resize_pty(h):
    """nerv-pty tracks the new size itself (SIGWINCH → shadow terminal), so
    after a resize the next key redraws a whole box at the new width, with
    the typed line intact under the cursor."""
    pane = h.window()
    h.split(pane)
    tmux("select-pane", "-t", pane)
    h.type(pane, TYPED)
    ok, lines = wait_for(pane, popup_up)
    if not ok:
        log("  popup never opened")
        dump(lines)
        return False
    def at(sel):
        # The footer moving to [sel/2] proves a paint after the resize,
        # not the box left over from before it.
        return lambda ls: box_whole(ls, ITEMS) and any(f"[{sel}/{ITEMS}]" in l for l in ls)

    tmux("resize-pane", "-Z", "-t", pane)             # grow
    h.key(pane, "Down")
    grown, lines = wait_for(pane, at(2))
    if not grown:
        log("  no redraw after growing the pane")
        dump(lines)
        return False
    tmux("resize-pane", "-Z", "-t", pane)             # shrink back
    h.key(pane, "Up")
    ok, lines = wait_for(pane, at(1))
    # capture-pane clips at the pane edge, so a box drawn for the old,
    # wider pane shows up as rows that lost their right border.
    fits = all(l.rstrip()[-1:] in "╮│┤╯" for l in box_rows(lines))
    cy = cursor_y(pane)
    kept = cy < len(lines) and lines[cy].startswith(h.prompt + "git c")
    if not (ok and fits and kept):
        log(f"  after shrink: redrawn={ok} fits={fits} kept={kept}")
        dump(lines)
    return ok and fits and kept


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
    ("bottom-grow", check_bottom_grow),
    ("detach-attach", check_detach_attach),
]
# Per-path: ZLE closes the popup on a resize; nerv-pty redraws it.
PATH_SCENARIOS = {
    "zle": [("resize", check_resize),
            ("resize-list-trap", lambda h: check_resize(h, "E2E_LIST_TRAP=1 "))],
    "pty": [("resize", check_resize_pty)],
}


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
    if not ZSH:
        log("zsh not on PATH")
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
        scenarios = SCENARIOS + PATH_SCENARIOS[args.path]
        for name, fn in scenarios:
            ok = fn(h)
            log(f"{args.path}/{name}: {'ok' if ok else 'FAIL'}")
            if not ok:
                failed.append(name)
    finally:
        h.stop()
    if failed:
        log(f"FAIL — {', '.join(failed)}")
        return 1
    log(f"PASS — {len(scenarios)} scenarios on the {args.path} path")
    return 0


if __name__ == "__main__":
    sys.exit(main())
