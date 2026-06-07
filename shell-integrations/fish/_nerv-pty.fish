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

        # PreExec is intentionally omitted for parity with the bash MVP;
        # the next prompt's StartPrompt resets the shadow term's state.
        # (fish's fish_preexec event would make a clean PreExec trivial —
        # a follow-up, PLAN §6.2.)
    end
else if set -q NERV_PTY
    # ---- Top-level: re-exec under nerv-pty -------------------------------
    # Only shim a real interactive TTY; leave scripts/pipes alone.
    if status is-interactive
        set -l __nerv_pty_bin nerv-pty
        if set -q NERV_PTY_BIN
            set __nerv_pty_bin $NERV_PTY_BIN
        end
        if command -v $__nerv_pty_bin >/dev/null 2>&1
            exec $__nerv_pty_bin -- $SHELL
        else
            echo "[nerv] NERV_PTY=1 set but nerv-pty binary not found — falling back to no shim." >&2
            echo "[nerv] Install nerv-pty (it ships with nerv >= 0.2) or unset NERV_PTY." >&2
        end
    end
end
