# adyton fish glue — emitted by `adyton init fish` (spec §5.1).
# Model output only reaches the command line by assignment (commandline -r),
# never executed or parsed as code.
#
# fish function names allow only letters/digits/underscores, so the zsh
# `?`/`??`/`???` shortcuts become `ai`/`aifix`/`aiask` here. Ctrl-G is the
# shared primary UX (in-place, empty line → fix).

if not set -q ADYTON_BIN
    set -gx ADYTON_BIN {{ADYTON}}
end

if not set -q ADYTON_TTY
    set -l t (tty 2>/dev/null | string replace -ra '[^A-Za-z0-9]' _)
    if test -n "$t"
        set -gx ADYTON_TTY $t
    else
        set -gx ADYTON_TTY pid_$fish_pid
    end
end

if set -q XDG_CACHE_HOME
    set -g __adyton_cache $XDG_CACHE_HOME/adyton
else
    set -g __adyton_cache $HOME/.cache/adyton
end
set -g __adyton_session $__adyton_cache/session-$ADYTON_TTY.log
begin
    set -l old (umask)
    umask 077
    mkdir -p $__adyton_cache
    umask $old
end

# --- session log + failure state (spec §5.3, §7.3) -------------------------
# fish provides $status (exit) and $CMD_DURATION (ms) for the just-run command;
# $argv[1] is the command line. Capture $status first, before anything resets it.

function __adyton_postexec --on-event fish_postexec
    set -l ret $status
    set -l cmd $argv[1]
    test -n "$cmd"; or return
    set -l now (date +%s)
    printf '%s\t%s\t%s\t%s\t%s\n' $now $ret $CMD_DURATION "$PWD" "$cmd" >>$__adyton_session
    if test $ret -ne 0
        set -l old (umask)
        umask 077
        printf 'exit=%s\ncmd=%s\ncwd=%s\nts=%s\nshell=fish\n' $ret "$cmd" "$PWD" $now >$__adyton_cache/last
        umask $old
    end
    set __adyton_count (math "$__adyton_count + 1")
    if test (math "$__adyton_count % 50") -eq 0; and test -f $__adyton_session
        set -l n (wc -l <$__adyton_session | string trim)
        if test "$n" -gt 200
            tail -n 100 $__adyton_session >$__adyton_session.tmp
            and mv -f $__adyton_session.tmp $__adyton_session
        end
    end
end
set -g __adyton_count 0

function __adyton_cleanup --on-event fish_exit
    test -n "$__adyton_session"; and rm -f $__adyton_session
end

# --- Ctrl-G widget (spec §5.1): buffer → adyton → buffer, in place ----------

function __adyton_widget
    set -l query (commandline)
    set -l result
    if test -n "$query"
        set result ("$ADYTON_BIN" suggest --shell fish -- "$query" | string collect)
    else
        set result ("$ADYTON_BIN" fix --shell fish | string collect)
    end
    if test $status -eq 0; and test -n "$result"
        commandline -r -- $result
        commandline -C (string length -- $result)
    end
    commandline -f repaint
end
bind \cg __adyton_widget

# --- ai / aifix / aiask: fish's ?/??/??? (print to terminal) ---------------

function ai
    "$ADYTON_BIN" suggest --shell fish -- $argv
end
function aifix
    "$ADYTON_BIN" fix --shell fish
end
function aiask
    "$ADYTON_BIN" ask --shell fish -- $argv
end
