# adyton zsh glue — emitted by `adyton init zsh` (spec §5).
# Model output only ever reaches the command line by assignment
# (BUFFER=… / print -z --); it is never executed or parsed as code.

# Baked in at emission; override with ADYTON_BIN for relocation or tests.
: "${ADYTON_BIN:={{ADYTON}}}"

export ADYTON_TTY="${${TTY#/dev/}//[^A-Za-z0-9]/_}"

typeset -g __adyton_cache="${XDG_CACHE_HOME:-$HOME/.cache}/adyton"
typeset -g __adyton_session="$__adyton_cache/session-$ADYTON_TTY.log"
typeset -g __adyton_cmd=""
typeset -gF __adyton_start=0
typeset -gi __adyton_count=0

() {
  local old_umask
  old_umask="$(umask)"
  umask 077
  mkdir -p -- "$__adyton_cache"
  : >> "$__adyton_session"
  umask "$old_umask"
}

zmodload zsh/datetime 2>/dev/null

# --- session log + failure state (spec §5.3, §7.3) -------------------------

__adyton_preexec() {
  __adyton_cmd="${1//$'\n'/ }"
  __adyton_start="$EPOCHREALTIME"
}

__adyton_precmd() {
  local ret=$?
  [[ -n "$__adyton_cmd" ]] || return 0
  local now dur_ms
  now="$EPOCHSECONDS"
  printf -v dur_ms '%.0f' $(( (EPOCHREALTIME - __adyton_start) * 1000 ))
  print -r -- "$now"$'\t'"$ret"$'\t'"$dur_ms"$'\t'"$PWD"$'\t'"$__adyton_cmd" \
    >> "$__adyton_session"
  if (( ret != 0 )); then
    # Subshell umask so `last` (may hold a sensitive command line) is 0600
    # regardless of the ambient umask (spec §5.3).
    ( umask 077
      print -r -- "exit=$ret"$'\n'"cmd=$__adyton_cmd"$'\n'"cwd=$PWD"$'\n'"ts=$now"$'\n'"shell=zsh" \
        > "$__adyton_cache/last" )
  fi
  __adyton_cmd=""
  # spec §7.3: rotate at 200 records, checked sparsely to stay off the
  # per-prompt hot path
  if (( ++__adyton_count % 50 == 0 )); then
    local -a __adyton_lines
    __adyton_lines=("${(@f)$(<"$__adyton_session")}")
    if (( ${#__adyton_lines} > 200 )); then
      print -rl -- "${(@)__adyton_lines[-100,-1]}" > "$__adyton_session"
    fi
  fi
  return 0
}

__adyton_zshexit() {
  [[ -n "$__adyton_session" ]] && rm -f -- "$__adyton_session"
}

# --- Ctrl-G widget (spec §5.1): buffer → adyton → buffer --------------------

__adyton_widget() {
  emulate -L zsh
  local query="$BUFFER" result ret
  zle -I
  if [[ -n "$query" ]]; then
    result="$("$ADYTON_BIN" suggest --shell zsh -- "$query")"
  else
    result="$("$ADYTON_BIN" fix --shell zsh)"
  fi
  ret=$?
  if (( ret == 0 )) && [[ -n "$result" ]]; then
    BUFFER="$result"
    CURSOR="${#BUFFER}"
  fi
  zle redisplay
  return 0
}

# --- ? / ?? fallbacks: push onto the editing buffer stack -------------------
# Aliases, not functions named ?/??: zsh globs the command word before
# function lookup, so a bare ? dies with "no matches found". Alias expansion
# beats globbing, and noglob keeps glob characters inside the query literal.

__adyton_suggest_push() {
  local result
  result="$("$ADYTON_BIN" suggest --shell zsh -- "$*")" || return $?
  [[ -n "$result" ]] && print -z -- "$result"
}

__adyton_fix_push() {
  local result
  result="$("$ADYTON_BIN" fix --shell zsh)" || return $?
  [[ -n "$result" ]] && print -z -- "$result"
}

# Prose Q&A: streams to the terminal, never touches the buffer.
__adyton_ask() {
  "$ADYTON_BIN" ask --shell zsh -- "$*"
}

alias -- '?'='noglob __adyton_suggest_push'
alias -- '??'='noglob __adyton_fix_push'
alias -- '???'='noglob __adyton_ask'

autoload -Uz add-zsh-hook
add-zsh-hook preexec __adyton_preexec
add-zsh-hook precmd __adyton_precmd
add-zsh-hook zshexit __adyton_zshexit

zle -N adyton-widget __adyton_widget
bindkey '^G' adyton-widget
