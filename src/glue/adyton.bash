# adyton bash glue — emitted by `adyton init bash` (spec §5.1).
# Model output only reaches the command line by assignment (READLINE_LINE=…),
# never executed or parsed as code.
#
# bash cannot name a command `?` (invalid identifier), so the zsh `?`/`??`/`???`
# shortcuts become `ai`/`aifix`/`aiask` here. Ctrl-G is the shared primary UX;
# it needs bash >= 4 for READLINE_LINE (macOS /bin/bash is 3.2 — install a newer
# bash for the widget; the functions and hooks work everywhere).

: "${ADYTON_BIN:={{ADYTON}}}"

if [[ -z "${ADYTON_TTY:-}" ]]; then
  __adyton_tty="$(tty 2>/dev/null)"
  export ADYTON_TTY="${__adyton_tty//[^A-Za-z0-9]/_}"
  [[ "$ADYTON_TTY" == *[A-Za-z0-9]* ]] || export ADYTON_TTY="pid_$$"
fi

__adyton_cache="${XDG_CACHE_HOME:-$HOME/.cache}/adyton"
__adyton_session="$__adyton_cache/session-$ADYTON_TTY.log"
__adyton_start=""
__adyton_seen=""
__adyton_count=0

( umask 077; mkdir -p -- "$__adyton_cache" )

# --- session log + failure state (spec §5.3, §7.3) -------------------------

# DEBUG fires before each command; record the start of the first one after a
# prompt (skip the prompt hook and completion).
__adyton_preexec() {
  [[ -n "${COMP_LINE:-}" ]] && return
  [[ "$BASH_COMMAND" == "${PROMPT_COMMAND:-}" ]] && return
  [[ -n "$__adyton_seen" ]] && return
  __adyton_seen=1
  __adyton_start="${EPOCHREALTIME:-}"
}

__adyton_precmd() {
  local ret=$?
  local cmd dur_ms=0 now
  read -r _ cmd <<< "$(history 1)"
  __adyton_seen=""
  [[ -n "$cmd" ]] || return 0
  now="$(date +%s)"
  # EPOCHREALTIME (bash 5+) is microseconds with a locale decimal mark; strip
  # it to integer µs. On bash 4 it is unset, so duration stays best-effort 0.
  if [[ -n "$__adyton_start" && -n "${EPOCHREALTIME:-}" ]]; then
    dur_ms=$(( (${EPOCHREALTIME/[.,]/} - ${__adyton_start/[.,]/}) / 1000 ))
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "$now" "$ret" "$dur_ms" "$PWD" "$cmd" >> "$__adyton_session"
  if (( ret != 0 )); then
    # Subshell umask so `last` (may hold a sensitive command line) is 0600
    # regardless of the ambient umask (spec §5.3).
    ( umask 077
      printf 'exit=%s\ncmd=%s\ncwd=%s\nts=%s\nshell=bash\n' \
        "$ret" "$cmd" "$PWD" "$now" > "$__adyton_cache/last" )
  fi
  if (( ++__adyton_count % 50 == 0 )) && [[ -f "$__adyton_session" ]]; then
    local lines; lines="$(wc -l < "$__adyton_session")"
    if (( lines > 200 )); then
      tail -n 100 "$__adyton_session" > "$__adyton_session.tmp" \
        && mv -f "$__adyton_session.tmp" "$__adyton_session"
    fi
  fi
  return 0
}

trap '__adyton_preexec' DEBUG
case "${PROMPT_COMMAND:-}" in
  *__adyton_precmd*) ;;
  *) PROMPT_COMMAND="__adyton_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac

# --- Ctrl-G widget (spec §5.1): buffer → adyton → buffer, in place ----------

__adyton_widget() {
  local out
  if [[ -n "$READLINE_LINE" ]]; then
    out="$("$ADYTON_BIN" suggest --shell bash -- "$READLINE_LINE")" || return
  else
    out="$("$ADYTON_BIN" fix --shell bash)" || return
  fi
  if [[ -n "$out" ]]; then
    READLINE_LINE="$out"
    READLINE_POINT="${#READLINE_LINE}"
  fi
}

if ((BASH_VERSINFO[0] >= 4)); then
  bind -x '"\C-g": __adyton_widget' 2>/dev/null
fi

# --- ai / aifix / aiask: bash's ?/??/??? (print to stdout — no buffer API) --

ai()    { "$ADYTON_BIN" suggest --shell bash -- "$@"; }
aifix() { "$ADYTON_BIN" fix --shell bash; }
aiask() { "$ADYTON_BIN" ask --shell bash -- "$@"; }
