#!/usr/bin/env python3
"""eval-history-replay.py — replay a zsh history file through nerv's ranking.

Usage: python3 scripts/eval-history-replay.py [--histfile ~/.zsh_history]
                                              [--nerv PATH] [--split 0.8]

Imports the first `split` of the commands into an isolated daemon (temp
HOME), then walks the rest in order: asks for the ghost at two prefix
lengths and for the empty-prompt prediction, scores each against the
command actually run, then records it. Baselines are what zsh alone
offers: the newest `$history` prefix match, and whatever followed the
previous command last time.

Prints aggregates only, never command text: the history is private.
History files carry no directory, so the directory term is inert here.
"""
import argparse, atexit, os, shutil, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--histfile", default=os.path.expanduser("~/.zsh_history"))
ap.add_argument("--nerv", default=shutil.which("nerv") or "nerv")
ap.add_argument("--split", type=float, default=0.8)
args = ap.parse_args()
NERV, HIST = args.nerv, args.histfile


def env_for(home):
    e = dict(os.environ)
    e.update(HOME=home, NERV_AUTOSTART="0", NERV_FRECENCY_FILE="-", NERV_MISSES_FILE="-",
             NERV_HISTORY_FILE=os.path.join(home, "history.tsv"))
    e.pop("NERV_SOCKET", None)
    return e


def start(home):
    """A daemon under `home`, stopped and removed (with the copy of the
    history in it) however the script ends."""
    e = env_for(home)
    atexit.register(shutil.rmtree, home, True)
    atexit.register(stop, e)
    subprocess.run([NERV, "start"], env=e, capture_output=True)
    sock = os.path.join(home, "Library/Caches/nerv/nervd.sock")
    for _ in range(100):
        if os.path.exists(sock):
            break
        time.sleep(0.1)
    else:
        sys.exit(f"no daemon socket after 10 s: {NERV} start failed")
    time.sleep(0.5)
    return e


def stop(e):
    subprocess.run([NERV, "stop"], env=e, capture_output=True)


def unesc(f):
    return f.replace("\\t", "\t").replace("\\n", "\n").replace("\\\\", "\\")


def rows(home):
    p = os.path.join(home, "history.tsv")
    for line in open(p, encoding="utf-8", errors="replace"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) >= 5:
            yield parts


# 1. Parse the real history with nerv's own importer.
a = tempfile.mkdtemp(prefix="nerv-eval-a-")
ea = start(a)
subprocess.run([NERV, "_import-history", HIST], env=ea, capture_output=True)
time.sleep(0.5)
stop(ea)
cmds = [unesc(r[4]) for r in rows(a)]
cmds = [c for c in cmds if c and "\n" not in c and "\t" not in c]
shutil.rmtree(a, ignore_errors=True)
n = len(cmds)
split = int(n * args.split)
train, test = cmds[:split], cmds[split:]

# 2. Train store.
b = tempfile.mkdtemp(prefix="nerv-eval-b-")
trainfile = os.path.join(b, "train_history")
with open(trainfile, "w") as f:
    for c in train:
        f.write(c + "\n")
eb = start(b)
subprocess.run([NERV, "_import-history", trainfile], env=eb, capture_output=True)
time.sleep(0.5)


def ghost_row(out):
    first = out.split("\n", 1)[0]
    if first.startswith("\x1fghost\t"):
        return first[len("\x1fghost\t"):]
    return None


def nerv_ghost(typed, prev):
    e = dict(eb, NERV_PREV=prev, NERV_TYPED=typed)
    out = subprocess.run([NERV, "_complete", typed, str(len(typed))], env=e,
                         capture_output=True, text=True).stdout
    return ghost_row(out)


def nerv_predict(prev):
    e = dict(eb, NERV_PREV=prev)
    out = subprocess.run([NERV, "_predict"], env=e, capture_output=True, text=True).stdout
    return ghost_row(out)


def baseline(typed, seen):
    for c in reversed(seen):
        if c.startswith(typed) and c != typed:
            return c
    return None


def prefixes(cmd):
    out = {"2ch": cmd[:2] if len(cmd) > 2 else None}
    w = cmd.split(" ", 1)
    out["word+1"] = (w[0] + " " + w[1][:1]) if len(w) == 2 and len(w[1]) > 1 else None
    return out


stats = {k: {"n": 0, "nerv_hit": 0, "base_hit": 0, "nerv_shown": 0, "base_shown": 0,
             "nerv_only": 0, "base_only": 0} for k in ("2ch", "word+1")}
pred = {"n": 0, "shown": 0, "hit": 0, "base_shown": 0, "base_hit": 0}
seen = list(train)
prev = train[-1] if train else ""
for cmd in test:
    # Prediction vs "repeat what followed prev last time".
    pred["n"] += 1
    p = nerv_predict(prev)
    if p:
        pred["shown"] += 1
        pred["hit"] += p == cmd
    last_follow = None
    for i in range(len(seen) - 1, 0, -1):
        if seen[i - 1] == prev:
            last_follow = seen[i]
            break
    pred["base_shown"] += last_follow is not None
    pred["base_hit"] += last_follow == cmd
    for k, typed in prefixes(cmd).items():
        if not typed:
            continue
        s = stats[k]
        s["n"] += 1
        g = nerv_ghost(typed, prev)
        g = (typed + g[len(typed):]) if g and g.startswith(typed) else None
        bl = baseline(typed, seen)
        s["nerv_shown"] += g is not None
        s["base_shown"] += bl is not None
        nh, bh = g == cmd, bl == cmd
        s["nerv_hit"] += nh
        s["base_hit"] += bh
        s["nerv_only"] += nh and not bh
        s["base_only"] += bh and not nh
    data = (cmd + "\0\0" + prev + "\0").encode()
    subprocess.run([NERV, "_record-cmd", "--cwd", "/tmp", "--exit", "0"], env=eb, input=data,
                   capture_output=True)
    seen.append(cmd)
    prev = cmd
stop(eb)
shutil.rmtree(b, ignore_errors=True)

print(f"commands parsed {n}, train {len(train)}, test {len(test)}, distinct {len(set(cmds))}")
for k, s in stats.items():
    if s["n"]:
        print(f"ghost@{k}: n={s['n']}  exact hit nerv {s['nerv_hit']/s['n']:.1%} vs $history {s['base_hit']/s['n']:.1%}"
              f"  (shown {s['nerv_shown']/s['n']:.0%} vs {s['base_shown']/s['n']:.0%};"
              f" nerv-only {s['nerv_only']}, base-only {s['base_only']})")
if pred["n"]:
    print(f"predict: n={pred['n']} shown {pred['shown']/pred['n']:.1%} hit {pred['hit']/pred['n']:.1%}"
          f" precision {pred['hit']/max(pred['shown'],1):.1%}  | last-follower baseline"
          f" shown {pred['base_shown']/pred['n']:.1%} hit {pred['base_hit']/pred['n']:.1%}"
          f" precision {pred['base_hit']/max(pred['base_shown'],1):.1%}")
