# demo.tape — the README demo GIF. Drive with ./scripts/record-demo.sh
# (it builds the throwaway HOME + demo project this tape assumes).
#
# Beats, in order:
#   1. `git ch`      — ghost text + popup with descriptions and arg hints
#   2. Right arrow   — accept the ghost
#   3. `git checkout ` — LIVE branches, read from the demo repo
#   4. `npm run `    — scripts lifted straight out of package.json
#   5. `cd s`        — folders, with item counts as descriptions
# Keep it under ~25s: this plays on autoloop in the README.

Output "docs/assets/demo.gif"

Set Shell zsh
Set FontSize 20
Set Width 1000
Set Height 500
Set Padding 20
Set Framerate 24
Set TypingSpeed 90ms
Set Theme "Catppuccin Mocha"

# VHS starts zsh with --no-rcs (deliberate: user dotfiles must not leak
# into a recording), so the demo rc has to be sourced by hand. Do it
# between Hide/Show so the setup never reaches a frame.
Hide
Type "source $HOME/.zshrc"
Enter
Sleep 3s
Show

Sleep 800ms

# 1 — static spec completion: subcommands, descriptions, arg hints.
Type "git ch"
Sleep 2.2s

# 2 — inline ghost accepted with Right arrow.
Right
Sleep 1.2s

# 3 — dynamic: the branch list comes from THIS repo, not the spec.
#     Enter on a branch INSERTS it (insert-only for non-directories —
#     no surprise execution); the second Enter is the user running it.
Type " "
Sleep 2.4s
Down
Sleep 900ms
Enter
Sleep 1.2s
Enter
Sleep 1.8s

Hide
Type "clear"
Enter
Sleep 400ms
Show

# 4 — package.json scripts, read natively (no Node on the hot path).
Type "npm run "
Sleep 2.4s
Escape
Ctrl+U
Sleep 500ms

# 5 — folders, described by what's inside them (bare `cd `, so the list
#     shows several — `cd s` would narrow to a single match).
Type "cd "
Sleep 2.6s
Escape
Ctrl+U
Sleep 1.2s
