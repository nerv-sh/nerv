#!/usr/bin/env python3
"""E2E: executed-command history and the history-ranked ghost (_nerv.zsh).

The widget records every command it runs (preexec → precmd) into
$NERV_HISTORY_FILE, honouring zsh's own ignore rules, and seeds that
file from $HISTFILE on the first prompt. The inline ghost comes from
the ranked history (docs/history-suggestions.md).

Each scenario gets a fresh HOME, daemon and zsh on a pty rendered
through pyte.

Run from repo root:
  cargo build -p nerv-cli -p nerv-daemon && uv run -q --with pyte python3 scripts/e2e-zle-history.py
Requires: cargo-built debug binaries (default features), zsh, pyte.

The widget is baked into the binary with include_str! (nerv-cli/src/main.rs),
so editing _nerv.zsh changes nothing here until you rebuild.
"""
import os, shutil, signal, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from zle_harness import PROMPT, Shell  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NERV = os.path.join(REPO, "target", "debug", "nerv")
SPECS = os.path.join(REPO, "crates", "nerv-engine", "tests", "fixtures", "specs")


def log(msg):
    print(f"[e2e-history] {msg}", flush=True)


def rows(home):
    try:
        with open(os.path.join(home, "history.tsv")) as f:
            return [l.rstrip("\n").split("\t") for l in f if l.strip()]
    except FileNotFoundError:
        return []


def commands(home):
    return [r[4] for r in rows(home)]


def wait_for(pred, seconds=8.0):
    # Recording is fire-and-forget (`&!`); on a loaded machine the row can
    # land a few seconds after the prompt.
    deadline = time.time() + seconds
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(0.1)
    return pred()


def run(sh, line):
    sh.type(line + "\n")
    # A predicted ghost may follow the fresh prompt, so wait on the cursor
    # sitting right after it rather than on a bare prompt line.
    sh.settle(lambda: sh.cursor_line().startswith(PROMPT.rstrip())
              and sh.screen.cursor.x == len(PROMPT))


class Env:
    """A fresh HOME with its own daemon, history file and zshrc."""

    def __init__(self, histfile_lines=(), extra_rc="", history_rows=(), pre_rc=""):
        self.home = tempfile.mkdtemp(prefix="nerv-hist-")
        zdot = os.path.join(self.home, "zdot")
        os.makedirs(zdot)
        self.work = os.path.realpath(os.path.join(self.home, "work"))
        os.makedirs(self.work)
        for sub in ("a", "b"):
            os.makedirs(os.path.join(self.work, sub))
        # Pre-seeded ranked history: (command, cwd relative to work, prev).
        # One imported-style row (empty cwd) marks it as already seeded, so
        # the first prompt's import does not add $HISTFILE to it.
        if history_rows == "empty":
            open(os.path.join(self.home, "history.tsv"), "w").close()
            os.chmod(os.path.join(self.home, "history.tsv"), 0o600)
        elif history_rows:
            with open(os.path.join(self.home, "history.tsv"), "w") as f:
                f.write("1700000000\t0\t\t\tseeded\t\n")
                for i, (cmd, cwd, prev) in enumerate(history_rows):
                    d = os.path.join(self.work, cwd) if cwd else self.work
                    f.write(f"{int(time.time()) - 100 + i}\t0\t{d}\t{prev}\t{cmd}\t\n")
            os.chmod(os.path.join(self.home, "history.tsv"), 0o600)
        histfile = os.path.join(self.home, ".zsh_history")
        with open(histfile, "w") as f:
            for line in histfile_lines:
                f.write(line + "\n")
        # Every nerv invocation from the widget logs its argv here, so a
        # check can prove command text never rides argv.
        self.argv_log = os.path.join(self.home, "argv.log")
        wrapper = os.path.join(self.home, "nerv-wrap")
        with open(wrapper, "w") as f:
            f.write(f'#!/bin/sh\nprintf "%s\\n" "$*" >> {self.argv_log}\nexec {NERV} "$@"\n')
        os.chmod(wrapper, 0o755)
        with open(os.path.join(zdot, ".zshrc"), "w") as f:
            f.write(f"HISTFILE={histfile}\nHISTSIZE=1000\nSAVEHIST=1000\n")
            f.write("setopt hist_ignore_space\n")
            # Emacs keys whatever $EDITOR says: in the vi keymap ^? is
            # vi-backward-delete-char, which the widget does not wrap.
            f.write("bindkey -e\n")
            f.write(f"PS1='{PROMPT}'\n")
            # Plugins loaded before nerv, as oh-my-zsh does ahead of the
            # block `nerv init` appends.
            f.write(pre_rc)
            f.write(f'eval "$({NERV} init zsh)"\n')
            f.write(f"__NERV_BIN={wrapper}\n")
            f.write(extra_rc)
        env = dict(os.environ)
        env.pop("TMUX", None)
        env.update(HOME=self.home, ZDOTDIR=zdot, NERV_SPECS_DIR=SPECS, NERV_AUTOSTART="0",
                   NERV_PATH_SCAN="0", TERM="xterm-256color",
                   NERV_HISTORY_FILE=os.path.join(self.home, "history.tsv"),
                   NERV_FRECENCY_FILE="-", NERV_MISSES_FILE="-")
        self.env = env
        subprocess.run([NERV, "start"], env=env, capture_output=True)
        wait_for(lambda: subprocess.run([NERV, "_complete", "git c", "5"], env=env,
                                        capture_output=True, text=True).stdout.strip() != "",
                 10)

    def shell(self):
        return Shell(self.env, self.work)

    def close(self):
        subprocess.run([NERV, "stop"], env=self.env, capture_output=True)
        shutil.rmtree(self.home, ignore_errors=True)


def check_record(e):
    sh = e.shell()
    try:
        run(sh, "echo first")
        run(sh, "false")
        run(sh, "ls -d .")
    finally:
        sh.close()
    ok = wait_for(lambda: commands(e.home)[-3:] == ["echo first", "false", "ls -d ."])
    rs = rows(e.home)
    if not ok:
        log(f"  rows: {rs}")
        return False
    first, fail, last = rs[-3:]
    ok = (first[2] == e.work and first[1] == "0" and fail[1] == "1"
            and fail[3] == "echo first" and last[3] == "false")
    if not ok:
        log(f"  rows: {rs[-3:]} work={e.work}")
    return ok


def check_privacy(e):
    sh = e.shell()
    try:
        run(sh, "echo before")
        run(sh, " echo SPACESECRET")
        run(sh, "echo IGNORESECRET")
        # A private session: `fc -p` pushes a history with no file.
        run(sh, "fc -p")
        run(sh, "echo PRIVATESECRET")
        run(sh, "fc -P")
        run(sh, "echo after")
    finally:
        sh.close()
    wait_for(lambda: "echo after" in commands(e.home))
    text = open(os.path.join(e.home, "history.tsv")).read()
    after = [r for r in rows(e.home) if r[4] == "echo after"]
    leaked = "SECRET" in text
    # The ignored commands broke the chain: `echo after` has no prev.
    chained = not after or after[0][3] != ""
    if leaked or chained:
        log(f"  rows: {rows(e.home)}")
    return not leaked and not chained and bool(after)


def check_argv(e):
    sh = e.shell()
    try:
        run(sh, "echo ARGVMARKER")
    finally:
        sh.close()
    wait_for(lambda: "echo ARGVMARKER" in commands(e.home))
    recorded = "echo ARGVMARKER" in commands(e.home)
    try:
        argv = open(e.argv_log).read()
    except FileNotFoundError:
        argv = ""
    # Over the socket neither the keystrokes nor the record fork `nerv` at
    # all; whatever does run (the import) must not carry the text.
    if not recorded or "ARGVMARKER" in argv:
        log(f"  recorded={recorded} argv log: {argv!r}")
    return recorded and "ARGVMARKER" not in argv


def check_fork_path(e):
    # NERV_SOCKET=0: the same ghost and the same record through `nerv`
    # processes, with the command text on stdin, not argv.
    sh = e.shell()
    try:
        run(sh, "echo FORKMARKER one")
        ghost = ghost_after(sh, "echo FORKMARKER ")
    finally:
        sh.close()
    recorded = wait_for(lambda: "echo FORKMARKER one" in commands(e.home))
    try:
        argv = open(e.argv_log).read()
    except FileNotFoundError:
        argv = ""
    record = [l for l in argv.splitlines() if l.startswith("_record-cmd")]
    ok = recorded and bool(record) and not any("FORKMARKER" in l for l in record) and ghost == "one"
    if not ok:
        log(f"  recorded={recorded} ghost={ghost!r} record argv={record!r}")
    return ok


def check_import(e):
    sh = e.shell()
    sh.close()
    ok = wait_for(lambda: commands(e.home)[:3] == ["git add .", "git commit -m wip", "echo 한글"])
    if not ok:
        log(f"  rows: {rows(e.home)}")
        return False
    doctor = subprocess.run([NERV, "doctor"], env=e.env, capture_output=True, text=True).stdout
    row = [l for l in doctor.splitlines() if "history" in l]
    if not any("3 commands" in l for l in row):
        log(f"  doctor: {row}")
        return False
    return True


def ghost_after(sh, typed):
    """Type `typed` and return the grey text after the cursor."""
    sh.type(typed)
    sh.settle(lambda: len(sh.cursor_line()) > len(PROMPT + typed), 3)
    return sh.cursor_line()[len(PROMPT + typed):].strip()


def check_dir_ghost(e):
    # `echo alpha two` is the newest match, which is what `$history`
    # would offer everywhere; in dir a the ranking must pick `one`.
    sh = e.shell()
    try:
        run(sh, "cd a")
        ghost = ghost_after(sh, "echo alpha ")
        ok = ghost == "one"
        sh.type("\x15")  # ^U
        run(sh, "cd ../b")
        ghost_b = ghost_after(sh, "echo alpha ")
        ok = ok and ghost_b == "two"
        if not ok:
            log(f"  a={ghost!r} b={ghost_b!r}")
        return ok
    finally:
        sh.close()


def check_seq_ghost(e):
    sh = e.shell()
    try:
        plain = ghost_after(sh, "echo second-")
        sh.type("\x15")
        run(sh, "echo prep")
        after = ghost_after(sh, "echo second-")
        ok = plain == "y" and after == "x"
        if not ok:
            log(f"  plain={plain!r} after-prep={after!r}")
        return ok
    finally:
        sh.close()


def check_ranked_nothing_wins(e):
    # $HISTFILE knows `ls -la zzsecretdir`; the ranked history does not.
    # The daemon's "no match" must not be painted over with `$history`.
    sh = e.shell()
    try:
        ghost = ghost_after(sh, "ls -la zz")
        if ghost:
            log(f"  ghost={ghost!r}")
        return ghost == ""
    finally:
        sh.close()


def check_empty_history_fallback(e):
    # history.tsv exists but holds nothing (so no import either): the
    # daemon sends no ghost row, and `$history` still paints after a space.
    sh = e.shell()
    try:
        ghost = ghost_after(sh, "ls -la zz")
        if ghost != "top":
            log(f"  ghost={ghost!r}")
        return ghost == "top"
    finally:
        sh.close()


def prompt_ghost(sh):
    """Grey text on the fresh, empty prompt line."""
    sh.settle(lambda: len(sh.cursor_line()) > len(PROMPT), 2)
    return sh.cursor_line()[len(PROMPT):].strip()


def check_predict(e):
    sh = e.shell()
    try:
        run(sh, "echo prep")
        ghost = prompt_ghost(sh)
        if ghost != "echo next-thing":
            log(f"  after prep: {ghost!r}")
            return False
        # Right-arrow accepts it into the line.
        sh.key("\x1b[C")
        sh.settle(lambda: sh.at_typed("echo next-thing"), 2)
        if not sh.at_typed("echo next-thing"):
            log(f"  accept: line={sh.cursor_line()!r} x={sh.screen.cursor.x}")
            return False
        # Typing replaces it; deleting back to empty brings it back.
        sh.type("\x15")
        sh.type("x")
        sh.type("\x7f")
        back = prompt_ghost(sh)
        if back != "echo next-thing":
            log(f"  after clear: {back!r}")
            return False
        # Enter on the empty line runs nothing and leaves no ghost text in
        # the scrollback.
        sh.type("\x15")
        run(sh, "")
        left = sh.screen.display[sh.screen.cursor.y - 1].rstrip()
        if left != PROMPT.rstrip():
            log(f"  scrollback line: {left!r}")
            return False
        # Up-arrow recall and a paste fill the line without the widget;
        # the prediction must not glue onto them.
        sh.key("\x1b[A")
        sh.settle(None, 0.5)
        up = sh.cursor_line()
        sh.key("\x1b[C")
        sh.settle(None, 0.3)
        up_after = sh.cursor_line()
        sh.type("\x15")
        sh.key("\x1b[200~ls\x1b[201~")
        sh.settle(None, 0.5)
        paste = sh.cursor_line()
        if "next-thing" in up + up_after + paste:
            log(f"  up={up!r} up+right={up_after!r} paste={paste!r}")
            return False
        sh.type("\x15")
        # Esc dismisses a showing prediction.
        sh.type("x")
        sh.type("\x7f")
        esc_before = prompt_ghost(sh)
        sh.key("\x1b")
        sh.settle(None, 0.4)
        if esc_before != "echo next-thing" or sh.cursor_line().rstrip() != PROMPT.rstrip():
            log(f"  esc: before={esc_before!r} after={sh.cursor_line()!r}")
            return False
        # A habit seen once is not predicted.
        run(sh, "echo lonely")
        once = prompt_ghost(sh)
        if once:
            log(f"  after lonely: {once!r}")
        return once == ""
    finally:
        sh.close()


def check_predict_off(e):
    sh = e.shell()
    try:
        run(sh, "echo prep")
        ghost = prompt_ghost(sh)
        if ghost:
            log(f"  NERV_PREDICT=0 still predicted {ghost!r}")
        return ghost == ""
    finally:
        sh.close()


def check_predict_hung_daemon(_e):
    # A daemon that accepts and never answers must cost the prediction,
    # not the prompt: `_predict` gives up within its 150 ms bound.
    import socket
    home = tempfile.mkdtemp(prefix="nerv-hung-")
    sock_dir = os.path.join(home, "Library", "Caches", "nerv")
    os.makedirs(sock_dir)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(os.path.join(sock_dir, "nervd.sock"))
    srv.listen(8)
    try:
        env = dict(os.environ, HOME=home, NERV_PREV="echo prep")
        t = time.time()
        r = subprocess.run([NERV, "_predict"], env=env, capture_output=True, text=True, timeout=10)
        took = time.time() - t
        ok = took < 1.0 and r.returncode != 0 and "ghost" not in r.stdout
        if not ok:
            log(f"  took={took:.2f}s rc={r.returncode} out={r.stdout!r}")
        return ok
    finally:
        srv.close()
        shutil.rmtree(home, ignore_errors=True)


GIT_SUBS = ("status", "log", "checkout", "commit")


def popup_order(sh, typed):
    """Type `typed` and return the git subcommands in popup row order."""
    sh.type(typed)
    sh.settle(lambda: sum(any(w in l for w in GIT_SUBS) for l in sh.box()) >= 4, 3)
    order = []
    for line in sh.box():
        for w in GIT_SUBS:
            if f" {w} " in f" {line} " and w not in order:
                order.append(w)
                break
    return order


def check_popup_typed(e):
    # `git status` was typed by hand, never picked from the popup; it
    # still rises to the top of `git `.
    sh = e.shell()
    try:
        order = popup_order(sh, "git ")
        if not order or order[0] != "status":
            log(f"  order={order}")
        return bool(order) and order[0] == "status"
    finally:
        sh.close()


def check_popup_dir(e):
    sh = e.shell()
    try:
        run(sh, "cd a")
        in_a = popup_order(sh, "git ")
        sh.type("\x15")
        run(sh, "cd ../b")
        in_b = popup_order(sh, "git ")
        ok = in_a[:1] == ["log"] and in_b[:1] == ["checkout"]
        if not ok:
            log(f"  a={in_a} b={in_b}")
        return ok
    finally:
        sh.close()


def check_popup_seq(e):
    # `git push` is run most, but right after `echo prep` it is `commit`.
    sh = e.shell()
    try:
        plain = popup_order(sh, "git ")
        sh.type("\x15")
        run(sh, "echo prep")
        after = popup_order(sh, "git ")
        ok = plain[:1] == ["log"] and after[:1] == ["commit"]
        if not ok:
            log(f"  plain={plain} after-prep={after}")
        return ok
    finally:
        sh.close()


def check_popup_history_rows(e):
    # No spec or generator knows `feature-x`; the history does.
    sh = e.shell()
    try:
        sh.type("git checkout f")
        sh.settle(lambda: any("feature-x" in l for l in sh.box()), 3)
        box = sh.box()
        items = [l for l in box if "│" in l and "[" not in l and l.strip("│ ").strip()]
        # `feature-x` is the only row, so it is the selected one, and the
        # selected row's description sits in the footer.
        footer = [l for l in box if "[1/1]" in l]
        ok = (len(items) == 1 and "feature-x" in items[0]
              and bool(footer) and "history" in footer[0])
        if not ok:
            log(f"  box={sh.box()}")
        return ok
    finally:
        sh.close()


def check_sock_read_skips_stale_replies(e):
    # The widget's reader against a fake daemon: a late reply to request 4
    # arrives before the reply to 5 and must be skipped, rows and all; a
    # daemon that never answers costs the read bound, not the prompt.
    import socket, threading
    path = os.path.join(e.home, "fake.sock")
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    srv.listen(4)
    conns = []

    def serve():
        c, _ = srv.accept()
        conns.append(c)
        c.sendall(b"stale-row\t\t\t\t\n\x1fend\t4\t0\nfresh-row\t\t\t\t\n\x1fend\t5\t4\n")
        c2, _ = srv.accept()  # the hung one: accepts, never writes
        conns.append(c2)

    threading.Thread(target=serve, daemon=True).start()
    sh = e.shell()
    try:
        run(sh, f"zsocket {path}; __NERV_SOCK_FD=$REPLY; __nerv_sock_read 5 1;"
                " print -r -- \"code=$REPLY n=${#__NERV_SOCK_LINES} first=${__NERV_SOCK_LINES[1]%%$'\\t'*}\"")
        got = [l for l in sh.screen.display if l.startswith("code=")]
        # Timed inside zsh: the harness's own settling is not the widget's.
        run(sh, f"zsocket {path}; __NERV_SOCK_FD=$REPLY; t0=$EPOCHREALTIME;"
                " __nerv_sock_read 1 0.3; rc=$?;"
                " printf 'hung=%s fd=%s ms=%.0f\\n' $rc $__NERV_SOCK_FD $(( (EPOCHREALTIME - t0) * 1000 ))")
        hung = [l.strip() for l in sh.screen.display if l.startswith("hung=")]
        took = int(hung[0].rsplit("ms=", 1)[1]) / 1000 if hung else 99
        ok = (got and got[0].strip() == "code=4 n=1 first=fresh-row"
              and hung and hung[0].startswith("hung=1 fd=0 ") and took < 1)
        if not ok:
            log(f"  got={got} hung={hung} took={took:.2f}")
        return ok
    finally:
        sh.close()
        for c in conns:
            c.close()
        srv.close()


def check_hung_socket_falls_back(e):
    # The widget's socket points at a daemon that accepts and never
    # answers: the keystroke waits out the read bound, then the fork path
    # brings the same rows.
    import socket
    path = os.path.join(e.home, "hung.sock")
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    srv.listen(4)
    sh = e.shell()
    try:
        run(sh, f"zsocket {path}; __NERV_SOCK_FD=$REPLY")
        t = time.time()
        sh.type("git ")
        sh.settle(lambda: any("checkout" in l for l in sh.box()), 6)
        took = time.time() - t
        ok = any("checkout" in l for l in sh.box()) and took >= 1.5
        if not ok:
            log(f"  took={took:.2f} box={sh.box()}")
        return ok
    finally:
        sh.close()
        srv.close()


def check_stderr_survives_commands(e):
    # Closing the socket before each command must not take the shell's
    # stderr with it (a bare `exec {fd}<&- 2>/dev/null` does, for good).
    sh = e.shell()
    try:
        run(sh, "echo one")
        run(sh, "echo two")
        run(sh, "nosuchcmdzz")
        ok = any("command not found" in l for l in sh.screen.display)
        if not ok:
            log(f"  screen={[l.rstrip() for l in sh.screen.display if l.strip()]}")
        return ok
    finally:
        sh.close()


def check_legacy_ghost(e):
    # The zsh `$history` ghost still works: recall `pwd pbcopy` from a
    # bare `pwd`.
    sh = e.shell()
    try:
        sh.type("pwd")
        sh.settle(lambda: "pbcopy" in sh.cursor_line(), 3)
        ok = "pwd pbcopy" in sh.cursor_line()
        if not ok:
            log(f"  line: {sh.cursor_line()!r}")
        return ok
    finally:
        sh.close()


# zsh-autosuggestions: a checkout named by NERV_E2E_AUTOSUGGEST, or a
# usual install. The stub only defines the function the widget looks for,
# so the yield is checked without it; the real plugin, when found, adds
# the checks on its own ghost (both load orders).
AUTOSUGGEST = next((p for p in [
    os.environ.get("NERV_E2E_AUTOSUGGEST", ""),
    os.path.expanduser("~/.oh-my-zsh/custom/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh"),
    "/opt/homebrew/share/zsh-autosuggestions/zsh-autosuggestions.zsh",
    "/usr/share/zsh-autosuggestions/zsh-autosuggestions.zsh",
] if p and os.path.exists(p)), None)
AUTOSUGGEST_STUB = "_zsh_autosuggest_start() { :; }\n"
YIELD_NOTICE = "zsh-autosuggestions is loaded"
AUTOSUGGEST_ENV = {
    "histfile_lines": ["git check-plugin-ghost"],
    "history_rows": [("git checkout main", "", "")] * 3
    + [("echo prep", "", ""), ("echo next-thing", "", "echo prep")] * 2,
}


def check_autosuggest_yield(e, real):
    # nerv's own ghost stays off: no prediction after `echo prep`, and
    # `git chec` would ghost `kout` from the popup's top row and the ranked
    # history's `git checkout main`. The popup itself stays, and the notice
    # is printed once per shell. With the stub nothing else paints, so any
    # ghost is nerv's; the real plugin overwrites nerv on the keys it wraps.
    sh = e.shell()
    try:
        run(sh, "echo prep")
        predicted = prompt_ghost(sh)
        sh.type("git chec")
        sh.settle(lambda: any("checkout" in l for l in sh.box()), 3)
        popup = any("checkout" in l for l in sh.box())
        after = sh.cursor_line()[len(PROMPT + "git chec"):].strip()
        notices = sum(YIELD_NOTICE in l for l in sh.screen.display)
        ok = predicted == "" and popup and notices == 1
        spaced = ""
        if real:
            # The plugin's ghost comes from $history, and Right-arrow
            # accepts it through the plugin's own forward-char wrapper.
            ok = ok and after == "k-plugin-ghost"
            sh.key("\x1b[C")
            sh.settle(lambda: sh.at_typed("git check-plugin-ghost"), 2)
            ok = ok and sh.at_typed("git check-plugin-ghost")
            # Space is nerv's own widget, which the plugin does not wrap:
            # its ghost must follow the new line, not stay glued on.
            sh.type("\x15git")
            sh.settle(lambda: "check-plugin-ghost" in sh.cursor_line(), 2)
            sh.type(" ")
            sh.settle(lambda: sh.cursor_line() == PROMPT + "git check-plugin-ghost", 2)
            spaced = sh.cursor_line()
            ok = ok and spaced == PROMPT + "git check-plugin-ghost" and sh.at_typed("git ")
        else:
            ok = ok and after == ""
        if not ok:
            log(f"  real={real} predicted={predicted!r} popup={popup} after={after!r}"
                f" notices={notices} spaced={spaced!r} line={sh.cursor_line()!r}")
        return ok
    finally:
        sh.close()


def check_no_plugin_no_notice(e):
    sh = e.shell()
    try:
        run(sh, "echo one")
        return not any(YIELD_NOTICE in l for l in sh.screen.display)
    finally:
        sh.close()


SCENARIOS = [
    ("record", check_record, {}),
    ("privacy", check_privacy, {"extra_rc": "HISTORY_IGNORE='*IGNORESECRET*'\n"}),
    ("argv", check_argv, {}),
    ("fork-path", check_fork_path, {"extra_rc": "NERV_SOCKET=0\n"}),
    ("stderr-intact", check_stderr_survives_commands, {}),
    ("sock-read-stale", check_sock_read_skips_stale_replies, {}),
    # No prediction: its 150 ms bound would consume the hung socket first.
    ("hung-socket-fallback", check_hung_socket_falls_back, {"extra_rc": "NERV_PREDICT=0\n"}),
    ("import", check_import,
     {"histfile_lines": [": 1700000000:0;git add .", ": 1700000005:0;git commit -m wip",
                         ": 1700000009:0;echo 한글"]}),
    ("legacy-ghost", check_legacy_ghost, {"histfile_lines": ["cd /tmp", "pwd pbcopy"]}),
    ("dir-ghost", check_dir_ghost,
     {"history_rows": [("echo alpha one", "a", ""), ("echo alpha one", "a", ""),
                       ("echo alpha two", "b", ""), ("echo alpha two", "b", "")]}),
    ("seq-ghost", check_seq_ghost,
     {"history_rows": [("echo second-y", "", "")] * 3
      + [("echo prep", "", ""), ("echo second-x", "", "echo prep")] * 2}),
    ("popup-typed", check_popup_typed, {"history_rows": [("git status", "", "")]}),
    ("popup-dir", check_popup_dir,
     {"history_rows": [("git log", "a", "")] * 3 + [("git checkout .", "b", "")] * 3}),
    ("popup-seq", check_popup_seq,
     {"history_rows": [("git log", "", "")] * 4
      + [("echo prep", "", ""), ("git commit -m wip", "", "echo prep")] * 2}),
    ("popup-history-rows", check_popup_history_rows,
     {"history_rows": [("git checkout feature-x", "", "")] * 2}),
    ("predict", check_predict,
     {"history_rows": [("echo prep", "", ""), ("echo next-thing", "", "echo prep")] * 2
      + [("echo lonely", "", ""), ("echo after-lonely", "", "echo lonely")]}),
    ("predict-off", check_predict_off,
     {"history_rows": [("echo prep", "", ""), ("echo next-thing", "", "echo prep")] * 2,
      "extra_rc": "NERV_PREDICT=0\n"}),
    ("predict-hung-daemon", check_predict_hung_daemon, {}),
    ("empty-history-fallback", check_empty_history_fallback,
     {"histfile_lines": ["ls -la zztop"], "history_rows": "empty"}),
    ("autosuggest-yield-stub", lambda e: check_autosuggest_yield(e, False),
     dict(AUTOSUGGEST_ENV, pre_rc=AUTOSUGGEST_STUB)),
    # The block `nerv init` writes is usually last, but a plugin manager
    # may still load the plugin after it.
    ("autosuggest-yield-late", lambda e: check_autosuggest_yield(e, False),
     dict(AUTOSUGGEST_ENV, extra_rc=AUTOSUGGEST_STUB)),
    ("no-plugin-no-notice", check_no_plugin_no_notice, {}),
    ("ranked-nothing-wins", check_ranked_nothing_wins,
     {"histfile_lines": ["ls -la zzsecretdir"], "history_rows": [("echo unrelated", "", "")]}),
]

if AUTOSUGGEST:
    SCENARIOS += [
        ("autosuggest-yield-plugin", lambda e: check_autosuggest_yield(e, True),
         dict(AUTOSUGGEST_ENV, pre_rc=f"source {AUTOSUGGEST}\n")),
        ("autosuggest-yield-plugin-late", lambda e: check_autosuggest_yield(e, True),
         dict(AUTOSUGGEST_ENV, extra_rc=f"source {AUTOSUGGEST}\n")),
    ]


def main():
    if not os.path.exists(NERV):
        log(f"missing binary: {NERV} — run `cargo build -p nerv-cli -p nerv-daemon`")
        return 2
    only = set(sys.argv[1:])
    failed = []
    for name, fn, kw in SCENARIOS:
        if only and name not in only:
            continue
        e = Env(**kw)
        try:
            ok = fn(e)
        finally:
            e.close()
        log(f"{'PASS' if ok else 'FAIL'} {name}")
        if not ok:
            failed.append(name)
    if failed:
        log(f"FAIL — {', '.join(failed)}")
        return 1
    log("PASS — all scenarios")
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    sys.exit(main())
