#!/usr/bin/env python3
"""E2E smoke for command-name completion on the first token (_nerv.zsh).

Typing a command name is a completion like any other: `doc` offers
`docker` in the popup and Tab inserts it, while a name that is already
complete (`git`) offers nothing — the popup preselects its first row, so
a leftover row would make Enter run the wrong command. A shell alias
(`k`) counts as complete too, even though the daemon cannot see it. A
typo (`dokcer`) reaches the name it meant, which prefix matching never
could. The correction survives the space: `dokcer ps` still offers
`docker`, and accepting it rewrites only the command word (`sudo` and the
arguments stay). A word the shell itself runs — a function, a builtin —
never gets one, and no correction row paints a ghost. The history ghost
keeps its priority over the popup, and survives the daemon being down,
since it never needed the engine.

Run from repo root:  python3 scripts/e2e-zle-cmdname.py
Requires: cargo-built debug binaries, zsh on PATH.
"""

import fcntl
import json
import shutil
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


# Everything the shell wrote, for checks that span the whole session.
TRANSCRIPT = []


def _session_leader():
    # New session with the pty as its controlling terminal, like a real
    # terminal tab: without it zsh runs with job control off and the
    # job-notice check below could never fail.
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


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
    TRANSCRIPT.append(out)
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
        # A shell alias whose prefix matches no spec: typing `q` offers
        # only this row, so the footer paints its description.
        f.write("alias qk=qstat\n")
        # One edit from `docker`: a function the daemon cannot see.
        f.write("dockr() { :; }\n")
        # Expands to a typo: the engine corrects the *expanded* line, so
        # its span does not index what the user typed.
        f.write("alias dk=dokcer\n")
        # An alias sharing a spec subcommand's name: `git checko` must keep
        # the spec's own description, not the alias expansion.
        f.write("alias checkout=qcheckout\n")
        # Ctrl-X Ctrl-B writes the edit buffer to a file: the only way to
        # read what Enter left behind without parsing redraw escapes.
        f.write(f"__dump() {{ print -rn -- \"$BUFFER\" > {home}/buffer; }}\n")
        f.write("zle -N __dump; bindkey '^X^B' __dump\n")
        # An empty PATH keeps zsh's own completion from producing the
        # same word on a Tab that nerv failed to handle. `nerv` itself
        # is reached through the absolute NERV_BIN the init block sets.
        f.write(f"PATH={emptybin}\n")
        f.write(f'eval "$({NERV} init zsh)"\n')

    env = dict(os.environ)
    env["HOME"] = home
    env["ZDOTDIR"] = zdot
    # The fixture specs plus an `expo` stem: the builtin `export` is two
    # edits from it, which is the real-world false positive the widget
    # guard exists for.
    specs = os.path.join(home, "specs")
    shutil.copytree(SPECS, specs)
    with open(os.path.join(specs, "expo.json"), "w") as f:
        json.dump({"name": "expo", "description": "Expo CLI"}, f)
    # `hash` is a shell builtin (and not a reserved word) one edit from
    # the `bash` stem — the pair that proves the builtin guard runs.
    # A name no other case types, so a frecency row naming it can only
    # come from accepting a correction.
    with open(os.path.join(specs, "zzzspec.json"), "w") as f:
        json.dump({"name": "zzzspec", "description": "Correction target"}, f)
    # A command whose argument has a name (`dev`) and a longer name that
    # extends it (`dev:web`): the finished-token case for Enter.
    with open(os.path.join(specs, "pn.json"), "w") as f:
        json.dump(
            {"name": "pn", "args": [{"name": "script", "suggestions": ["dev", "dev:web"]}]},
            f,
        )
    with open(os.path.join(specs, "bash.json"), "w") as f:
        json.dump({"name": "bash", "description": "Bourne-again shell"}, f)
    # The bundle ships a `sudo` spec; without one here, typing `sudo `
    # would be a (fixture-only) miss.
    with open(os.path.join(specs, "sudo.json"), "w") as f:
        json.dump({"name": "sudo", "description": "Run as another user"}, f)
    env["NERV_SPECS_DIR"] = specs
    # A real file, not the "-" sentinel: a correction accept must record
    # nothing, and that is only provable where recording works.
    frecency = os.path.join(home, "frecency.tsv")
    env["NERV_FRECENCY_FILE"] = frecency
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
            preexec_fn=_session_leader,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=env,
            close_fds=True,
        )
        os.close(slave)
        pump(master, 2.0)  # reach prompt

        # Shell-name registration happens on the first `precmd` in a
        # background subshell; the next prompt is the earliest point it
        # is provably done (the hook checks the job, then detaches).
        os.write(master, b"\r")
        pump(master, 1.0)

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

        # 2b: a shell FUNCTION the daemon now knows (registered from
        # ${(k)functions} at the first precmd) is offered like any name.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"dock")
        text = pump(master, 1.5).decode(errors="replace")
        if "dockr" not in text:
            failures.append("shell function 'dockr' was not offered for 'dock'")
        log(f"'dock': function offered={'dockr' in text}")

        # 2c: a shell ALIAS is offered too, and its description names the
        # expansion (`alias → body`) — the widget already reads $aliases
        # for line expansion. `q` matches only the alias, so the row is
        # selected and the footer paints the description.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"q")
        text = pump(master, 1.5).decode(errors="replace")
        if "qk" not in text:
            failures.append("shell alias 'qk' was not offered for 'q'")
        if "alias → qstat" not in text:
            failures.append("alias row did not describe its expansion")
        log(f"'q': alias offered={'qk' in text} desc={'alias → qstat' in text}")

        # 2d: only shell-name rows take the alias description — a spec row
        # that happens to share an alias's name keeps its own.
        os.write(master, b"\x07\x15")
        pump(master, 0.6)
        os.write(master, b"git checko")
        text = pump(master, 1.5).decode(errors="replace")
        if "alias → qcheckout" in text:
            failures.append("spec row 'checkout' took the alias description")
        if "Switch branches" not in text:
            failures.append("'git checko' did not show the spec row's description")
        log(f"'git checko': spec desc kept={'Switch branches' in text and 'alias →' not in text}")

        # A typo reaches the name it meant. Prefix matching cannot: the
        # whole top of misses.tsv is transpositions like this one.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"dokcer")
        text = pump(master, 1.5).decode(errors="replace")
        if "docker" not in text or "did you mean" not in text:
            failures.append("typo 'dokcer' was not corrected to 'docker'")
        log(f"'dokcer': corrected={'did you mean' in text}")

        # Accepting a correction is the one first-token row whose
        # insertion is not an extension of what was typed, so the whole
        # token has to be replaced rather than appended to.
        os.write(master, b"\t")
        plain = strip_ansi(pump(master, 1.5))
        if b"docker" not in plain or b"dokcer" in plain.rsplit(b"\n", 1)[-1]:
            failures.append("Tab did not replace the typo with the correction")
        log(f"'dokcer'+Tab: replaced={b'docker' in plain}")

        # The correction survives the space. From the browsing position
        # (sentinel selected) the first Tab moves onto the row and the
        # second accepts it.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"dokcer ")
        text = pump(master, 1.5).decode(errors="replace")
        if "did you mean" not in text:
            failures.append("'dokcer ' (after the space) drew no correction")
        log(f"'dokcer ': corrected={'did you mean' in text}")

        # Accepting keeps the arguments and rewrites only the command
        # word, including behind a wrapper.
        for typed, want in ((b"dokcer ps", b"docker ps"), (b"sudo dokcer x", b"sudo docker x")):
            os.write(master, b"\x15")
            pump(master, 0.6)
            os.write(master, typed)
            text = pump(master, 1.5).decode(errors="replace")
            os.write(master, b"\t")
            last = strip_ansi(pump(master, 1.5)).rsplit(b"\r", 1)[-1]
            ok = "did you mean" in text and want in last and b"dokcer" not in last
            if not ok:
                failures.append(f"accepting the correction in {typed!r} gave {last!r}")
            log(f"{typed.decode()!r}+Tab: {last.decode(errors='replace').strip()!r}")

        # Accepting a correction records no frecency: the user did not
        # pick a completion, they fixed a typo.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"zzzspce x")
        pump(master, 1.5)
        os.write(master, b"\t")
        last = strip_ansi(pump(master, 1.5)).rsplit(b"\r", 1)[-1]
        if b"zzzspec x" not in last:
            failures.append(f"correction accept did not rewrite: {last!r}")
        log(f"'zzzspce x'+Tab: {last.decode(errors='replace').strip()!r}")

        # A correction row never paints a ghost: `docker` shares the `d`
        # being typed, so without the guard Right-arrow would append
        # `ocker` to the argument.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"dokcer d")
        pump(master, 1.5)
        os.write(master, b"\x1b[C")  # Right-arrow: accept a ghost if any
        last = strip_ansi(pump(master, 1.0)).rsplit(b"\r", 1)[-1]
        # Accepting a ghost repaints the buffer tail, so `ocker` (the
        # part of `docker` past the typed `d`) shows up here; with the
        # guard in place Right-arrow at end of line has nothing to accept
        # and the terminal stays quiet. Verified by mutation: removing
        # the guard in __nerv_set_ghost makes this line read `...ocker`.
        if b"ocker" in last:
            failures.append(f"a correction row painted a ghost: {last!r}")
        log(f"'dokcer d'+Right: {last.decode(errors='replace').strip()!r}")

        # Words the shell runs itself get no row once they are finished.
        # For the builtins the engine still offers a correction and the
        # widget silences it. `dockr` is different since slice 02: the
        # daemon now KNOWS the function (registered shell names), so the
        # engine itself refuses to correct it — a correction aimed at a
        # real function is always wrong.
        for typed, engine_offers in (
            (b"dockr x", False),
            (b"export FOO=1 ", True),
            (b"export", True),
            (b"hash x", True),
        ):
            line = typed.decode()
            direct = subprocess.run(
                [NERV, "_complete", line, str(len(line))],
                env=env,
                capture_output=True,
                text=True,
            ).stdout
            if engine_offers and "did you mean" not in direct:
                failures.append(f"engine drew no correction for {line!r} — check is vacuous")
            os.write(master, b"\x15")
            pump(master, 0.6)
            os.write(master, typed[:-1])
            pump(master, 1.5)
            os.write(master, typed[-1:])
            text = pump(master, 1.5).decode(errors="replace")
            if "did you mean" in text:
                failures.append(f"shell word in {line!r} drew a correction")
            log(f"{line!r}: engine={'did you mean' in direct} popup={'did you mean' in text}")

        # What Enter means. PATH is an empty directory, so a line that
        # actually runs prints "command not found" — that is the signal.
        def buffer_now():
            path = os.path.join(home, "buffer")
            if os.path.exists(path):
                os.remove(path)
            os.write(master, b"\x18\x02")
            pump(master, 0.6)
            with open(path) as f:
                return f.read()

        shown_before_enter = {}

        def enter_after(typed):
            os.write(master, b"\x15")
            pump(master, 0.6)
            # The last keystroke alone: earlier ones pass through states
            # (`dokcer ⎵` before `./`) that paint their own rows.
            os.write(master, typed[:-1])
            pump(master, 1.5)
            os.write(master, typed[-1:])
            shown_before_enter[typed] = pump(master, 1.5).decode(errors="replace")
            os.write(master, b"\r")
            return strip_ansi(pump(master, 1.5))

        # A lone correction is preselected at a word boundary (a space, or
        # a `/` the user is browsing): Enter fixes the line instead of
        # running the typo.
        for typed, want in ((b"dokcer ", "docker "), (b"dokcer ./", "docker ./")):
            out = enter_after(typed)
            ran = b"command not found" in out
            buf = buffer_now()
            # Selecting the fix must not take the "run it" row away.
            sentinel = "Immediately execute" in shown_before_enter[typed]
            if ran or buf != want or not sentinel:
                failures.append(f"{typed!r}+Enter left {buf!r} (ran={ran}, sentinel={sentinel})")
            log(f"{typed.decode()!r}+Enter: ran={ran} buffer={buf!r} sentinel={sentinel}")

        # Ordinary rows after the space keep the sentinel: Enter runs.
        out = enter_after(b"git ")
        if b"command not found" not in out:
            failures.append("'git '+Enter did not run the line")
        log(f"'git '+Enter: ran={b'command not found' in out}")

        # A token typed in full runs even though a longer name remains —
        # and the longer name is still on offer, which proves rc 4 was read
        # as a success rather than as a dead daemon.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"pn dev")
        shown = pump(master, 1.5).decode(errors="replace")
        if "dev:web" not in shown or "daemon not running" in shown:
            failures.append(f"'pn dev' lost its popup: {shown[-60:]!r}")
        log(f"'pn dev': popup={'dev:web' in shown}")
        out = enter_after(b"pn dev")
        if b"command not found" not in out:
            failures.append(f"'pn dev'+Enter did not run the line: {out[-60:]!r}")
        log(f"'pn dev'+Enter: ran={b'command not found' in out}")

        # A partial token still takes the first row.
        out = enter_after(b"pn de")
        ran = b"command not found" in out
        buf = buffer_now()
        if ran or buf != "pn dev ":
            failures.append(f"'pn de'+Enter left {buf!r} (ran={ran})")
        log(f"'pn de'+Enter: ran={ran} buffer={buf!r}")

        # A correction against an alias-expanded line indexes a line the
        # buffer does not have, so the row must be dropped.
        os.write(master, b"\x15")
        pump(master, 0.6)
        os.write(master, b"dk p")
        pump(master, 1.5)
        os.write(master, b"s")
        text = pump(master, 1.5).decode(errors="replace")
        if "did you mean" in text:
            failures.append("a correction survived alias expansion ('dk ps')")
        log(f"alias-expanded 'dk ps': correction={'did you mean' in text}")

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
        # Compare without escapes: zle redraws only the cells that changed
        # since the previous ghost (`pn dev` from the Enter cases), so the
        # colour codes land between letters of the mark.
        text = strip_ansi(pump(master, 2.0)).decode(errors="replace")
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

    # Registration must not swallow a daemon-down failure: a session that
    # reached its first prompt before the daemon retries on the next
    # precmd and registers as soon as an attempt succeeds.
    # NERV_AUTOSTART=0 keeps the harness in charge of the daemon so the
    # first attempt deterministically fails.
    log("retry session: daemon down at the first prompt")
    env2 = dict(env)
    env2["NERV_AUTOSTART"] = "0"
    master2 = None
    proc2 = None
    try:
        m2, s2 = pty.openpty()
        fcntl.ioctl(s2, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        proc2 = subprocess.Popen(
            ["/bin/zsh"],
            preexec_fn=_session_leader,
            stdin=s2,
            stdout=s2,
            stderr=s2,
            env=env2,
            close_fds=True,
        )
        os.close(s2)
        master2 = m2
        pump(m2, 2.0)  # first prompt: the registration attempt fails
        os.write(m2, b"dock")
        text = pump(m2, 1.5).decode(errors="replace")
        if "dockr" in text:
            failures.append("shell names registered despite the daemon being down")
        log(f"daemon down: function offered={'dockr' in text}")
        os.write(m2, b"\x15")

        subprocess.run([NERV, "start"], env=env, capture_output=True)
        time.sleep(1.0)
        os.write(m2, b"\r")  # next prompt → the hook retries
        pump(m2, 1.5)
        os.write(m2, b"dock")
        text = pump(m2, 1.5).decode(errors="replace")
        if "dockr" not in text:
            failures.append("registration was not retried on the next precmd")
        log(f"after retry: function offered={'dockr' in text}")
        # Ctrl-G closes the open popup first; otherwise the Enter below
        # would accept its row instead of running the empty line.
        os.write(m2, b"\x07\x15")

        # The names live in the daemon's memory: a restart (what `nerv
        # doctor` tells a skewed user to do) must be followed by a
        # re-registration at the next prompt, keyed on the new pid.
        subprocess.run([NERV, "stop"], env=env, capture_output=True)
        subprocess.run([NERV, "start"], env=env, capture_output=True)
        time.sleep(1.0)
        os.write(m2, b"\r")  # next prompt → new daemon pid → re-register
        pump(m2, 1.5)
        os.write(m2, b"dock")
        text = pump(m2, 1.5).decode(errors="replace")
        if "dockr" not in text:
            failures.append("shell names were not re-registered after a daemon restart")
        log(f"after restart: function offered={'dockr' in text}")
        os.write(m2, b"\x15exit\n")
        time.sleep(0.3)
    except OSError as e:
        failures.append(f"pty error (retry session): {e}")
    finally:
        if proc2 is not None:
            proc2.send_signal(signal.SIGTERM)
            try:
                proc2.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc2.kill()
        if master2 is not None:
            os.close(master2)
        subprocess.run([NERV, "stop"], env=env, capture_output=True)

    # No widget may leak shell diagnostics onto the terminal — a second
    # `local` on an existing variable prints `NAME=value` (v0.1.12 did this
    # on every accepted completion).
    # Escapes first: `ESC 8` (restore cursor) sits right before the dump,
    # and its `8` would hide a word boundary.
    # zsh prints the dump as `NAME=value` and ends the line; typed
    # assignments (`export FOO=1 `) never end a line on their own.
    leaked = re.findall(
        rb"(?<![A-Za-z0-9_])[A-Za-z_][A-Za-z0-9_]*=\S*\r\n",
        strip_ansi(b"".join(TRANSCRIPT)),
    )
    if leaked:
        failures.append(f"widget printed variable dumps: {leaked[:3]!r}")
    log(f"variable dumps: {len(leaked)}")

    # The registration runs as a background job; an interactive shell
    # must not announce it (`[1] 12345`, `[1]  + done …`) at prompts.
    jobs = re.findall(rb"\[\d+\]\s+(?:\+\s+)?(?:done|\d+)", strip_ansi(b"".join(TRANSCRIPT)))
    if jobs:
        failures.append(f"job-control notices leaked: {jobs[:3]!r}")
    log(f"job notices: {len(jobs)}")

    # 4: none of those keystrokes may be tallied as a missing spec —
    # except `dockr`: the direct engine probes above settle a shell
    # function the daemon now knows about (slice 02), and the misses
    # rows that leaves are slice 03's pruning target (error-states.md
    # §3.6.3 프루닝 조항).
    tally = env["NERV_MISSES_FILE"]
    recorded = ""
    if os.path.exists(tally):
        with open(tally) as f:
            recorded = f.read().strip()
    stray = [r for r in recorded.splitlines() if not r.startswith("dockr\t")]
    if stray:
        failures.append(f"first-token keystrokes were tallied: {stray!r}")
    log(f"misses.tsv: {recorded.splitlines() or '(empty)'}")

    # Accepting a completion records; accepting a correction must not.
    rows = ""
    if os.path.exists(frecency):
        with open(frecency) as f:
            rows = f.read().strip()
    if "docker" not in rows:
        failures.append(f"no frecency recorded at all — the check is vacuous: {rows!r}")
    if "zzzspec" in rows:
        failures.append(f"a correction accept was recorded as frecency: {rows!r}")
    log(f"frecency.tsv: {rows.splitlines() or '(empty)'}")

    if failures:
        for f in failures:
            log(f"FAIL — {f}")
        return 1
    log(
        "PASS — command-name popup + Tab insert, correction after the "
        "space (wrapper-safe, no ghost, shell words skipped), exact-name "
        "and alias silence, shell function·alias candidates (alias desc "
        "names the expansion, spec rows keep theirs), registration retried after the daemon "
        "came up and after it restarted, no job notices, ghost (with and without a daemon), clean tally"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
