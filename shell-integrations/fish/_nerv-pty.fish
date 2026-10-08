# _nerv-pty.fish — fish bootstrap for the figterm-style PTY shim.
#
# fish has no inline-completion widget hook nerv can drive directly, so
# (like bash) it reaches inline autocomplete ONLY through the PTY path
# (PLAN §6.2). Activated when the user exports `NERV_PTY=1`:
#
#   - Top-level (no NERV_PTY_SESSION_ID): re-exec under `nerv-pty`.
#   - Inner shell (NERV_PTY_SESSION_ID set by nerv-pty): install the
#     OSC 697 prompt markers so the shadow terminal sees the edit buffer.
#
# Mirrors _nerv-pty.bash / _nerv-pty.zsh, adapted to fish syntax. fish
# can't `return` at top level when sourced, so the branches are nested
# in if/else rather than early returns.

if set -q NERV_PTY_SESSION_ID
    # ---- Inner shell: install OSC 697 markers ----------------------------
    # Re-entry guard: config.fish / sourcing can run this twice.
    if not set -q _NERV_PTY_PROMPT_SET
        set -g _NERV_PTY_PROMPT_SET 1

        function __nerv_pty_osc
            builtin printf '\033]697;%s\007' $argv[1]
        end

        # precmd: fish emits the `fish_prompt` event right before drawing
        # the prompt. `Shell=fish` is REQUIRED — the shadow term gates its
        # edit-buffer reads on a recognized shell (can_send_edit_buffer),
        # so without it no ghost/popup ever shows.
        function __nerv_pty_precmd --on-event fish_prompt
            __nerv_pty_osc "Shell=fish"
            __nerv_pty_osc "Dir=$PWD"
            __nerv_pty_osc "TTY="(command tty 2>/dev/null)
            __nerv_pty_osc "PID=$fish_pid"
        end

        # Wrap fish_prompt so each prompt is bracketed by Start/End +
        # NewCmd (carrying the session id for correlation). Preserve the
        # user's prompt by copying it aside first; fall back to a plain
        # `> ` when the user has no fish_prompt defined.
        if functions -q fish_prompt
            functions --copy fish_prompt __nerv_pty_user_prompt
        else
            function __nerv_pty_user_prompt
                builtin printf '> '
            end
        end
        function fish_prompt
            __nerv_pty_osc StartPrompt
            __nerv_pty_user_prompt
            __nerv_pty_osc EndPrompt
            __nerv_pty_osc "NewCmd=$NERV_PTY_SESSION_ID"
        end

        # PreExec: fish fires fish_preexec right after the user submits a
        # command, before it runs. This tells the shadow term to stop
        # offering completions while the command executes. fish's event
        # is clean (no DEBUG-trap startup hazard), so we emit it directly.
        function __nerv_pty_preexec --on-event fish_preexec
            __nerv_pty_osc PreExec
        end
    end
else if set -q NERV_PTY
    # ---- Top-level: re-exec under nerv-pty -------------------------------
    # Only shim a real interactive TTY; leave scripts/pipes alone. stdin
    # AND stdout must be TTYs so a piped stdin keeps flowing through the
    # pipe instead of being swallowed by the wrapper. Mirrors the zsh
    # `-t 0/-t 1` and bash `-t 0/-t 1` guards.
    if status is-interactive; and test -t 0; and test -t 1
        set -l __nerv_pty_bin nerv-pty
        if set -q NERV_PTY_BIN
            set __nerv_pty_bin $NERV_PTY_BIN
        end
        if command -v $__nerv_pty_bin >/dev/null 2>&1
            # Autostart nervd before handing over — the shim talks to
            # the same UDS. `nerv start` is idempotent (socket probe):
            # quiet no-op when a daemon already serves. The CLI lives
            # next to nerv-pty; fall back to PATH. NERV_AUTOSTART=0 opts
            # out, same as _nerv.zsh.
            if test "$NERV_AUTOSTART" != 0
                set -l __nerv_cli (dirname $__nerv_pty_bin)/nerv
                if not test -x $__nerv_cli
                    set __nerv_cli nerv
                end
                $__nerv_cli start >/dev/null 2>&1 &
                disown 2>/dev/null
            end
            # Extra shell args ride along (mirrors zsh `"$@"` / bash `"$@"`).
            # Re-exec this fish, not the login $SHELL: a zsh-login user
            # who runs `fish` must land back in fish.
            set -l __nerv_self_shell (status fish-path 2>/dev/null)
            or set __nerv_self_shell fish
            exec "$__nerv_pty_bin" -- "$__nerv_self_shell" $argv
        else
            echo "[nerv] NERV_PTY=1 but nerv-pty not found — falling back to no shim (unset NERV_PTY or install nerv-pty)." >&2
        end
    end
end
