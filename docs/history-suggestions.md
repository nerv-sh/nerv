# History-ranked suggestions

Nerv learns from the commands you actually run. It uses them to pick the
inline ghost, predict the next command on an empty prompt, and nudge the
popup order. This document is the canonical source for what is recorded,
where it is kept, and how it is scored. The ranking follows
[deja](https://github.com/Giammarco-Ferranti/deja) (MIT), adapted to nerv's
ZLE widget and daemon.

## 1. What is recorded

The zsh widget records each command once it finishes. `preexec` captures the
line and `precmd` sends it:

| Field | Source |
|---|---|
| command | preexec `$1`, the line as typed. The ghost offers this text back. |
| expanded | preexec `$3`, zsh's alias-expanded form. Empty when it equals `command`. |
| cwd | `$PWD` at the prompt after the command. |
| exit | `$?`, read on the first line of the hook. |
| prev | The command run before this one in the same shell. |
| ts | The daemon's clock when the row arrives. |

**What is never recorded:** anything zsh itself would not remember.

- **Leading space.** The widget skips a command with a leading space or tab while `hist_ignore_space` is set. The CLI and the daemon drop any leading-space command, whatever the option says.
- **`HISTORY_IGNORE`.** A command matching `HISTORY_IGNORE` is skipped too. Only the widget can check it, because the pattern lives in the shell.
- **The chain breaks.** An ignored command is not kept as the next command's `prev`, and no adjacency is invented across it.
- **History off.** With the history mechanism off (preexec `$1` empty), nothing is recorded.
- **No history file.** Nothing is recorded while `$HISTFILE` is unset, empty or `/dev/null`: a private session, `unset HISTFILE`, or the history `fc -p` pushes.
- **Not covered: `zshaddhistory` hooks.** A hook that rejects a line does not stop nerv from recording it. At `precmd` the rejected line is still in `$history`; zsh drops it only when the next line comes in, so the widget cannot tell. Use a leading space or `HISTORY_IGNORE` for such commands.

**How command text travels.**

- **Normal path.** The text goes over `stdin` to `nerv _record-cmd`, NUL-separated, and never through argv, so `ps` never shows it.
- **Daemon down.** The CLI appends the row to the file itself, and the daemon reads it at its next start. This also covers a daemon too old to know the request, or one whose write failed.
- **Locking.** Every write takes `history.tsv.lock` (flock), so a CLI append cannot vanish under an import or compaction that replaces the file.

**First prompt.** On the first prompt of a shell, if `history.tsv` does not exist yet, the widget seeds it once from `$HISTFILE` (`nerv _import-history`). The import:

- reads both plain and `EXTENDED_HISTORY` lines;
- joins `\`-continued multi-line commands;
- un-metafies zsh's byte encoding, so non-ASCII commands import intact;
- goes before any command recorded ahead of it, because the first command can reach the daemon before the import does;
- runs once. Imported rows are the only rows with an empty cwd, so a history that holds any is already seeded.

## 2. Where it is kept

The file is `~/Library/Caches/nerv/history.tsv`, created `0600`. A file that
already exists with looser permissions is tightened back to `0600`.

- **Override.** `NERV_HISTORY_FILE` overrides the path. `-` means no file at all.
- **Row format.** One row per command:

  ```text
  <unix ts>\t<exit>\t<cwd>\t<prev>\t<command>\t<expanded>
  ```

  Fields escape `\`, tab, CR and newline. A row that does not parse is skipped on load and never stops the load.
- **Writes.** The file is append-only: one command is one `O_APPEND` write.
- **Compaction.** Past 100,000 rows the file is rewritten once, keeping the newest 80,000 rows.
- **Uninstall.** `nerv uninstall` removes the file along with the rest of the cache directory (`uninstall-spec.md` §2).
- **Deleting it.** To clear the history, delete the file and restart the daemon (`nerv stop && nerv start`).

**What the daemon keeps in memory.** It holds three aggregates, built at load
and updated on every record:

- run count and last-run time per command;
- runs per command per directory;
- how often each command directly followed each other command.

Strings are interned once. Loading a full 100,000-row history of all-distinct
commands grows the daemon's RSS by about 27 MiB (`history_100k_rss`). A keystroke
never reads the file. In the worst case, where all 100,000 commands extend the typed text and
each one carries a sequence boost, choosing the ghost takes 3–5 ms (the same test).

`nerv doctor` reports the row count (`history  N commands`), or a warning when the
file is not writable.

## 3. The ghost

The widget shows as ghost text the history command that best extends what
you typed. Only commands that strictly extend the typed line are candidates, and only
single-line ones without a tab: a ghost is painted on one row, and the reply
format turns a tab into a space. Each candidate is
scored:

| Term | Weight | Value |
|---|---|---|
| sequence | 0.5 | times it directly followed the previous command, divided by that command's most frequent follower |
| command-head sequence | 0.25 | the same, keyed on the previous command's first two words (`git add` for `git add src/x.rs`) |
| frecency | 0.4 | `ln(1 + runs) · exp(-age / 1 week)`, divided by the best among the candidates |
| directory | 0.3 | runs in the current directory, divided by runs in any recorded directory |

Ties go to the most recent run. A command that exited 127 ("command not found",
a typo) is recorded but never counted.

These are deja's weights. Its fuzzy term drops out because a ghost only ever
extends the typed text. The command-head term is nerv's own. Without it,
`git add .` and `git add src/x.rs` would be unrelated predecessors.

**What the widget sends.** With each keystroke's completion request the widget
sends:

- the typed line (`NERV_TYPED`);
- the previous command (`NERV_PREV`), in the environment and not argv.

**What the daemon returns.** The reply's first row is `\x1fghost\t<command>`.

| Reply | Ghost shown |
|---|---|
| A command | That command |
| Empty `<command>` | Nothing. The ranking found no match, and zsh's `$history` is not painted over it. |
| No ghost row at all | zsh's most recent `$history` prefix match, as before this feature. The history is empty, the daemon is older, or the daemon is down. |

A bare command word, typed before any space, never reaches the daemon and keeps
the `$history` ghost. The ghost row is printed only for a widget that sets
`NERV_TYPED`. A shell still running an older widget after an upgrade would
otherwise list the row in its popup.

## 4. Empty-prompt prediction

On a fresh prompt, before anything is typed, the widget asks once
(`nerv _predict`, from zle's `line-init` hook) for the command that usually
follows the one just run, and shows it as ghost text.

- **Habit threshold.** Only a command that followed the previous one at least twice is predicted. Frecency alone would put a ghost on every prompt, so it plays no part here.
- **Fallback key.** When the exact previous command has no such follower, its command head is tried (`git add` of `git add .`).
- **Ties.** Runs in the current directory decide first, then the most recent run.
- **Accept.** Right-arrow accepts the prediction.
- **Typing and deleting.** Typing replaces it. Deleting back to an empty line shows it again without a new request.
- **Enter.** Enter on the empty line runs nothing, and no ghost text is left in the scrollback. The same holds for any ghost that was not accepted.
- **Turning it off.** `NERV_PREDICT=0` turns prediction off. Esc or `^G` hides it for the current line.

## 5. Popup order

The popup's rows come from the spec engine. History only decides their
order. For a row the daemon looks at two signals.

**Picks.** How often and how recently you picked the row from the popup (`frecency.tsv`).

**Runs.** How often and how recently the recorded commands had that word in
that place:

- Matching uses the finished words of the line. `git ch` looks at what came after `git`, and `git checkout ` looks at what came after `git checkout`.
- Commands are compared in their alias-expanded form, so `g st` counts toward `git status`.
- In a compound line only the last segment counts (`make && git ch`).
- Words are compared by name. `--mode=fast` counts for `--mode=`, and `src` counts for `src/`.

score = 0.4 · frecency + 0.3 · directory + 0.5 · sequence + 0.25 · head sequence

| Term | Value |
|---|---|
| frecency | `ln(1 + n) · exp(-age / 1 week)` over picks plus runs, divided by the best row in the list |
| directory | runs of that word made in the current directory, divided by its runs in any recorded directory |
| sequence | runs of that word that directly followed the previous command, divided by the best row |
| head sequence | the same, keyed on the previous command's first two words |

The weights are the ghost's (§3). Right after `git add .`, `commit` tops the
`git ` popup when commits have followed `git add` before.

**Consequences.**

- A single pick or a single run already counts. The old popup model ignored a single pick.
- Typing `git status` by hand raises `status` just as picking it does.
- Rows with no history keep the engine's order below the ones that have some.
- `./` and `../` stay pinned to the top.
- zoxide's rows keep zoxide's own order.

**History rows.** The history can also add rows the spec does not have,
for example a branch no generator listed after `git checkout`, or a host typed after
`ssh` that `~/.ssh/config` does not list:

- at most 5, best frecency first;
- only words that extend what is being typed;
- never a word the spec already offers, and never one holding a quote, a backtick, `$`, a control operator or a redirection (`;|&<>()`); the history index splits on whitespace, so such a word is a piece of something larger;
- each reads `history` and is ranked with the rest.

There are no history rows next to a command-word correction, which the widget
recognises by its being the only row. There are none for a command no spec
covers either: that reply stays empty, so the spec-miss tally still counts it,
and the widget asks zsh's own completion (spec-conversion-policy §6.4). When zsh
supplies rows, they replace the reply.

**Sequence floor.** As with the prediction, a sequence counts only once it has
happened at least twice.

**Cost.** The words before the cursor stay the same for every keystroke of a word
(`git c`, `git ch`, … all ask about `git`), so the daemon caches the last lookup
until the history changes. Only the first keystroke after a space scans.

| Case (`history_100k_rss`) | Time |
|---|---|
| Cold scan, worst case: all 100,000 commands follow `git` | 8–12 ms |
| Cached | under 0.1 ms |

## 6. Socket transport

The widget no longer starts a `nerv` process per request. zsh's
`zsh/net/socket` module opens the daemon's socket once per shell and keeps it
open. Requests are one line each, in a text protocol that zsh can write
without a JSON encoder (`nerv_engine::wire`):

- fields are separated by `\x1f`, with the verb first;
- a field escapes `\` as `\\`, newline as `\n` and `\x1f` as `\u`;
- `complete` and `predict` replies are the same rows `nerv _complete` prints, then `\x1fend\t<seq>\t<exit code>`;
- `record` has no reply.

The daemon serves both protocols on the same socket. A line whose first field
is a known verb is a text request. Anything else takes the JSON path, which
answers garbage with a JSON error.

**Latency.** `scripts/bench-socket.zsh`, debug build, `git ch`, n=200:

| Path | p50 | p95 |
|---|---|---|
| Socket | 0.25 ms | 0.36 ms |
| Forking `nerv _complete` | 12.05 ms | 23.46 ms |

**Failure handling.**

- **Fork fallback.** Any failure falls back to the old path for that one request: no socket module, no daemon, a closed connection, or no reply within the bound (2 s for a completion, 150 ms for a prediction). The next request reconnects.
- **Older daemon.** A daemon from before the text protocol answers with a JSON error line. The widget sees it and uses the fork path for the rest of the shell's life.
- **Stale replies.** The end line carries the request's sequence number, so a late reply to a request the widget gave up on is skipped rather than read as the next one's.

**Connection lifetime.** The connection closes before each command runs
(`preexec`). A zsocket descriptor survives exec, so a long-running command
would otherwise hold the daemon connection. The record at the next prompt
reconnects.

**Oversized requests.** A request over the daemon's 256 KiB line cap is still
answered in the text protocol, from its head, so it never reads as an old
daemon. A command over 60,000 characters never goes over the socket: at up to
4 bytes a character it could pass the cap, so the widget sends it through
`_record-cmd`.

**Privacy.** Command text reaches no process's argv on this path, not even
the typed line.

**Turning it off.** `NERV_SOCKET=0` forces the fork path.

## 7. With zsh-autosuggestions

zsh-autosuggestions paints its ghost into the same slot (`POSTDISPLAY`) from
its own widget wrappers. Two writers would overwrite each other on every key,
so when it is loaded nerv leaves the ghost to it and keeps only the popup.

- **Detection.** At the first prompt the widget looks for the plugin's
  `_zsh_autosuggest_start` function. The check waits for the prompt because a
  plugin manager may load the plugin after nerv.
- **Notice.** The shell prints one line to stderr, once:
  `[nerv] zsh-autosuggestions is loaded: it keeps the inline ghost text, nerv shows only its popup.`
- **What nerv stops.** The ranked history ghost, the popup's top-row ghost
  and the empty-prompt prediction. Right-arrow falls through to
  `forward-char`, which the plugin wraps to accept its own ghost. Space is
  nerv's own widget, which the plugin does not wrap, so nerv asks the plugin
  for a new ghost after it (`autosuggest-fetch`).
- **Widget order.** The plugin wraps every widget at the first prompt, and
  again at each prompt unless `ZSH_AUTOSUGGEST_MANUAL_REBIND` is set. Nerv
  re-claims `self-insert`, `backward-delete-char` and `accept-line` at each
  prompt. When the plugin's wrapper already calls nerv's widget, nerv leaves it
  in place. Taking the widget back would freeze the plugin's ghost while
  typing.
