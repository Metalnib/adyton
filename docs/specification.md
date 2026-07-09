# Adyton — Specification (MVP / Phase 1)

**Status:** v2 (2026-07-08) · contracts only — rationale lives in [architecture.md](architecture.md) (decisions D1–D7)
**Binary/command name:** `adyton`

## 1. Toolchain & build

- Latest stable Rust (≥ 1.96, edition 2024 — the current latest edition; `rust-toolchain.toml`
  tracks `stable`). Targets: `aarch64-apple-darwin`, `x86_64/aarch64-unknown-linux-musl`.
- Dependencies (exhaustive; adding one requires a decision record in architecture.md):
  `ureq = "3"` (default features: rustls/ring + webpki-roots) · `miniserde = "0.1"` · `lexopt = "0.3"`.
- Forbidden: tokio or any async runtime, OpenSSL/native-tls, clap, serde (unless executing the
  D3 swap-back, which replaces miniserde entirely).
- Release profile: `opt-level="z"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip=true`.
  (`-Zbuild-std` optional, never required for release artifacts.)
- Concurrency: `std::thread` only. At most three threads in MVP: main (blocking request/stream),
  spinner (stderr overlay, §6), and a conditional cache-refresh thread (§7.1) that runs
  concurrently with the API request.

## 2. CLI surface

```
adyton init <zsh|bash|fish>              print shell glue for eval
adyton suggest [opts] -- <nl query...>   natural language → command
adyton fix [opts]                        fix last failed command (state file, §5.3)
adyton ask [opts] -- <question...>       prose Q&A with full context ("explain this error");
                                         streams the answer to stdout — NEVER buffer-inserted
                                         (glue alias: `???`)
adyton context refresh                   rebuild context cache now
adyton config get <key> | set <key> <value> | path
adyton config set-key <profile>          store api key in the macOS keychain (key via stdin — never argv);
                                         writes the matching api_key_cmd lookup into the profile
adyton config check [--profile <p>]      validate config + key resolution (prints key SOURCE, never the key)
adyton --version | --help
```

Options (suggest/fix): `--shell <zsh|bash|fish>` (escaping/dialect hint), `--profile <name>`
(provider profile), `--no-context` (query only), `--plain` (no spinner/stream on stderr).

### 2.1 I/O contract
- **stdout:** exactly the final command string, no trailing newline decoration, nothing else. May
  be multi-line only if the command itself is (e.g. heredoc).
- **stderr:** all progress/overlay/stream rendering; suppressed by `--plain` or when stderr is not
  a TTY.
- **stdin:** when not a TTY (piped/redirected), read as context (§7.5); a terminal stdin is left untouched.
- **Exit codes:** `0` success · `1` provider/HTTP error (message on stderr) · `2` usage error ·
  `3` config error · `4` missing/invalid API key · `5` no cached state for `fix`.
  Glue must place stdout into the buffer **only on exit 0**.

## 3. Configuration

File: `${XDG_CONFIG_HOME:-~/.config}/adyton/config` · flat `key = value`, `#` comments,
`[profile.<name>]` sections. Hand-rolled reader (~50 lines); **full TOML is out of scope** — if
ever needed, adopt the `toml` crate rather than growing the parser.

```ini
default_profile = local
timeout_seconds = 60          # whole-request ceiling
session_log_commands = 20     # command-log records sent (§7.3), 0 disables
scrollback_lines = 120        # terminal scrollback capture (§7.4), 0 disables
git_timeout_ms  = 200

[profile.local]
wire   = openai               # openai | anthropic
base_url = http://localhost:11434/v1
model  = qwen3:8b

[profile.claude]
wire     = anthropic
base_url = https://api.anthropic.com
model    = claude-sonnet-4-20250514
extra_headers = anthropic-beta: token-efficient-tools
```

Per-profile keys: `wire`, `base_url`, `model`, `api_key` (discouraged), `api_key_cmd` (executed
argv-style, stdout = key), `extra_headers` (repeatable), `max_tokens` (default 1024),
`token_param` (`max_tokens` | `max_completion_tokens`, default per wire), `temperature`.

**API-key resolution order:** `--api-key` flag → `ADYTON_API_KEY` → `ADYTON_<PROFILE>_API_KEY` →
`api_key_cmd` (e.g. `security find-generic-password -w -s adyton -a claude`) → `api_key` in file.
Key handling per architecture §6: in-memory only, zeroized after the request.
**macOS Keychain is supported day one** via `api_key_cmd`; `adyton config set-key <profile>` is
the write-side convenience (key read from stdin, stored via `security -i` stdin — the key is
never an argv of any process; `-U` semantics make re-running it a key rotation).

## 4. Wire adapters

One internal event stream both adapters emit: `TextDelta(String)` ·
`ToolCallDelta{index, id?, name?, args_fragment}` · `Done{stop_reason, usage?}` · `WireError{status, body}`.
All JSON (de)serialization lives in `src/wire/` (D3). Parse failures wrap the raw line:
`"unparseable chunk: <line>"`.

### 4.1 openai (OpenAI-compatible)
- `POST {base_url}/chat/completions` (base_url includes `/v1` where the server expects it).
- Headers: `Authorization: Bearer <key>` (omitted if no key — local runners), `Content-Type: application/json`, + `extra_headers`.
- Body: `{model, stream:true, messages:[{role,content}], <token_param>: n, temperature?}`.
- SSE: lines `data: <json>`; ignore empty/comment lines; terminate on `data: [DONE]` or EOF.
  Text at `choices[0].delta.content`; tool-call args accumulate from
  `choices[0].delta.tool_calls[i].function.arguments` (string fragments, concatenate by `index`);
  `finish_reason` → `Done`.
- Non-2xx → `WireError` with the full response body (never fed to the SSE parser).

### 4.2 anthropic (Messages API, native)
- `POST {base_url}/v1/messages`.
- Headers: `x-api-key: <key>`, `anthropic-version: 2023-06-01`, `Content-Type: application/json`, + `extra_headers`.
- Body: `{model, stream:true, max_tokens (required), system?, messages}` — system prompt is the
  top-level `system` field, not a message.
- SSE named events; dispatch on JSON `type`: `content_block_delta` with `delta.type=text_delta`
  → `TextDelta(delta.text)`; `input_json_delta` → `ToolCallDelta(args_fragment=delta.partial_json)`;
  `message_delta` carries `stop_reason`/`usage`; `message_stop` → `Done`. Ignore `ping`;
  `error` events → `WireError`. There is **no `[DONE]` sentinel**.

### 4.3 Transport (both)
ureq 3 blocking; read via `resp.body_mut().as_reader()` wrapped in `BufRead` line iteration
(incremental delivery verified — validation-log). Config `timeout_seconds` applies to the whole
request; connect timeout 5 s. No retries in MVP except one immediate retry on connection-reset
before first byte.

## 5. Shell glue contracts

Emitted by `adyton init <shell>` from strings embedded in the binary (D7). All three provide:
hotkey **Ctrl-G** (buffer → suggest → buffer), aliases `?` (suggest) and `??` (fix), and a
`precmd`-equivalent hook that captures failure state (§5.3). Placement is always by assignment —
never `eval` (D5). Glue inserts stdout only when exit code is 0.

### 5.1 Per-shell mechanisms (validated — see validation-log)
- **zsh** (≥5.0): widget via `zle -N adyton-widget` + `bindkey '^G'`; reads/writes `$BUFFER`,
  restores `$CURSOR`; alias path uses `print -z -- "$cmd"`. Failure capture:
  `precmd` hook reading `$?` + `fc -ln -1`.
- **bash** (**≥4.0 required**; macOS `/bin/bash` 3.2 unsupported — document MacPorts bash):
  widget via `bind -x '"\C-g": __adyton_widget'` reading/writing `$READLINE_LINE`/`$READLINE_POINT`.
  Alias path prints to stdout with a hint (no buffer API outside `bind -x`). Failure capture:
  `PROMPT_COMMAND` reading `$?` + `history 1`. `command_not_found_handle` runs in a subshell —
  it may only annotate the state file, never mutate shell state.
- **fish** (≥3.0; **claims single-pass — re-verify on a live fish before release**):
  `bind \cg __adyton_widget` using `commandline -b` (read) / `commandline -r --` (replace);
  abbreviation-style `?`; failure capture via `--on-event fish_postexec` reading `$status`.

### 5.2 Prompt-side contract
The widget takes the current buffer as the NL query when non-empty; with an empty buffer it
reuses the failure state (= `fix`). While waiting, the glue shows nothing itself — all feedback
is adyton's stderr overlay (§6).

### 5.3 Failure-state file
`${XDG_CACHE_HOME:-~/.cache}/adyton/last` — single flat record, written by glue hooks on every
prompt where `$? != 0`, consumed by `fix`:
`exit=<n>` · `cmd=<last command line>` · `cwd=<pwd>` · `ts=<unix>` · shell=`<zsh|bash|fish>`.
Written with `umask 077`. `fix` errors (exit 5) if absent or `ts` older than 10 minutes.

## 6. Overlay / streaming UX (stderr)

When stderr is a TTY and `--plain` is absent: single status line, rewritten in place
(`\r`+clear): spinner + phase (`context → request → streaming`), then the streamed command text
as `TextDelta`s arrive, dimmed; on `Done` the line is cleared (the real result goes to stdout).
No cursor addressing beyond `\r`/`\x1b[K`; no alternate screen; no persistent panel (phase 2+).
Spinner thread stops on first delta or error.

## 7. Context collection & redaction

Sent with `suggest`/`fix` unless `--no-context`:

| Source | Bound | Freshness |
|---|---|---|
| OS, arch, shell, adyton version | — | cached (§7.1) |
| package managers present + top-level packages | — | cached (§7.1) |
| cwd + session command log (§7.3) | `session_log_commands` (default 20) | live |
| failure state (`fix`) | 1 record | state file |
| git: branch, dirty flag, last commit subject | `git_timeout_ms` (default 200), skip on timeout/non-repo | live |
| terminal scrollback (§7.4) | `scrollback_lines` (default 120), tail-truncated to 8 KB | live, tiered |

### 7.1 Cache
`${XDG_CACHE_HOME:-~/.cache}/adyton/context` — flat kv. Probes run with explicit argv (no
shell), 300 ms timeout each, whole rebuild capped at 2 s.

**Refresh semantics (never blocks a request):** staleness checked by mtime. If older than 24 h,
`suggest`/`fix` **serve the stale data immediately** and spawn a refresh thread that runs while
the main thread performs the API request. The refresh writes `context.tmp` and `rename()`s it
atomically — if the process exits first (response finished), the abandoned write can never tear
the cache and the next invocation retries. Single-flight: refresh takes `context.lock`
(`O_EXCL`, considered stale after 60 s) and no-ops if held. A missing cache (first run) is the
one synchronous case: build it foreground, bounded at 2 s, else proceed with `os/arch/shell`
only. `adyton context refresh` is the manual foreground path (same lock).

Schema (machine facts only — the cache never stores history, env values, scrollback, or secrets;
those tiers are live-only and redacted at send time):

| Key | Content | Probe |
|---|---|---|
| `os`, `kernel`, `arch` | platform + userland flavor (BSD/GNU flags) | `uname`, `sw_vers` / `/etc/os-release` |
| `shell` | name + version (command dialect) | `$SHELL --version` |
| `adyton` | own version (cache invalidated on mismatch) | compiled-in |
| `hw` | core count, memory (`-j` style flags) | `sysctl` / `/proc` |
| `pkg.<manager>` | detected managers + versions (install suggestions use what exists). Detect list: **macOS** MacPorts, Homebrew · **Linux** apt/dpkg, dnf/yum, pacman, zypper, apk, nix, flatpak, snap · **language** cargo/rustup, uv, pip/pipx, npm/pnpm/yarn, gem, go | `which` + `--version` |
| `tools` | which of a curated ~40-entry list exist (`rg`, `eza`, `fd`, `jq`, …) — prefer installed modern tools | batched `which` |
| `packages.<manager>` | top-level/user-requested packages, one line per manager | `port installed requested`, `cargo install --list`, `npm -g ls`, `uv tool list`, … |

The `packages.*` probes are the slow ones (100 ms–seconds) and the reason this cache exists;
anything cheap stays live (§7 table).

### 7.2 Redaction (applies to history lines, env values are never sent at all)
Drop or mask before any bytes leave the process:
- Lines matching `(?i)(api[_-]?key|token|secret|password|passwd|bearer|authorization)\s*[=:]` → mask RHS.
- Values matching known key shapes (`sk-[A-Za-z0-9_-]{20,}`, `ghp_…`, `xox[bp]-…`, AWS
  `AKIA[0-9A-Z]{16}`, 40+ char base64/hex blobs) → `«redacted»`.
- `history` entries invoking `security`, `pass`, `op`, `gpg` → the full line is dropped.
The ruleset is one table in `src/context/redact.rs` with unit tests per pattern.

### 7.3 Session command log
The §5.3 glue hooks also append one record per executed command to
`${XDG_CACHE_HOME:-~/.cache}/adyton/session-<tty-id>.log` (umask 077). Line format:
**`ts<TAB>exit<TAB>duration_ms<TAB>cwd<TAB>cmd`** (cmd is the untouched remainder — tabs inside
it survive). The tty id is provided by the glue via **`ADYTON_TTY`** (the shell knows its
terminal for free; a subprocess cannot portably learn it) and is sanitized to
`[A-Za-z0-9]`+`_` before use in the filename. Rotated at 200 entries; deleted on shell exit
where the shell provides an exit hook (`zshexit`, fish `fish_exit`), else reaped by age (>24 h)
on next write. `suggest`/`fix` send the last `session_log_commands` records — command lines pass
the §7.2 redaction ruleset first (secret-tool invocations drop the whole record).

### 7.4 Command output (terminal scrollback) — tiered
The shell never sees command output; it exists only in the terminal/multiplexer. Sources tried
in order, first hit wins; on no hit this context is silently absent:
1. **Multiplexers:** tmux (`$TMUX` → `tmux capture-pane -p -S -<scrollback_lines>`), GNU screen
   (`$STY` → `screen -X hardcopy -h`), Zellij (`$ZELLIJ` → `zellij action dump-screen --full`).
2. **Host terminals with a permission-free CLI**, detected by the env var they set:
   WezTerm (`$WEZTERM_PANE` → `wezterm cli get-text --start-line=-<lines>`), kitty
   (`$KITTY_WINDOW_ID` → `kitty @ get-text --extent all`, requires `allow_remote_control`).
   Multiplexers take precedence over the host terminal. **iTerm2 is deliberately excluded** — its
   only capture paths (AppleScript / Python API) trigger a macOS Automation permission prompt,
   which we refuse to require; iTerm2 users use the pipe (§7.5).
3. **Re-run** (`adyton fix --rerun` only — never default, side effects are the user's explicit
   choice): re-executes the failed command with `2>&1`, capped at 8 KB, `timeout_seconds/4` box.
Captured output is: redacted (§7.2), tail-truncated to 8 KB (errors print last), and wrapped in
fenced delimiters with a system-prompt note that it is **untrusted program output, not
instructions** (prompt-injection surface; the never-auto-execute invariant is the backstop).
Global output-recording wrappers (`exec > >(tee)`, `script`) are **rejected**: they break TTY
detection for every program and record indiscriminately.

### 7.5 Piped input
When stdin is **not a TTY** (`lsof … | adyton ask …`, `adyton ask … < file`), adyton reads it as
context. This is the portable, zero-integration way to hand the model exact command output —
unlike scrollback (§7.4) it needs no tmux/terminal support, only a `|`. Read is bounded (256 KB
against a runaway pipe), then redacted (§7.2) and tail-truncated to 8 KB like scrollback. Rendered
as a `## piped input` prompt section, **last** (closest to the query), with untrusted-data framing
(the user selects it but does not author it — prompt-injection stance, D6). Applies to
`suggest`/`ask`/`fix`. Because it is *explicit*, piped input is included even under `--no-context`
(which suppresses only ambient gathering). A terminal stdin (interactive invocation, the ZLE
widget) is never read — reading would block on EOF.

## 8. Budgets (hard, CI-enforced)

| Metric | Budget | Measured baseline |
|---|---|---|
| Stripped binary, aarch64-apple-darwin | ≤ 1.5 MB | 1.24 MB |
| Stripped binary, linux-musl | ≤ 2.0 MB | aarch64: 1,642,776 B · x86_64: 1,962,784 B — both fully static (x86_64 verified by executing on glibc Debian) |
| Cold start → usage error (`adyton`) | ≤ 20 ms | ~2 ms |
| suggest local overhead (spawn → request written), warm cache | ≤ 20 ms | — |
| Runtime dep crates (whole tree, no proc-macros) | ≤ 40 | 35 |

## 9. Acceptance criteria (MVP definition of done)

1. `adyton suggest -- "find pdfs modified this week"` streams on stderr and prints exactly one
   command on stdout against: OpenAI, Anthropic (native), Ollama, LM Studio — same binary,
   config-only switch.
2. Wire adapters pass recorded-fixture tests: real captured SSE streams (happy path, tool-call
   fragments, mid-stream error event, 401/429/500 bodies) — fixtures re-recorded from live
   endpoints, which also re-verifies the single-pass protocol claims in the validation log.
3. Ctrl-G round-trip works in zsh 5.9, bash 5.x, fish 3.x: type NL → widget → command replaces
   the line, cursor at end, nothing executed. `?`/`??` aliases work; bash alias prints with hint.
4. `??` reproduces a failed command's fix using the state file; exit 5 when state is stale/absent.
5. Redaction unit tests pass; a history line containing `export OPENAI_API_KEY=sk-…` provably
   never appears in the request body (integration test asserts on the serialized body).
6. Budgets in §8 enforced in CI (size gate + startup gate on both platforms).
7. No `eval` anywhere in emitted glue (CI greps the `init` output).

## 10. Open items

- fish glue verification on a live fish (validation-log, open item 2).
- musl size measurement → tighten §8 budget.
- Default hotkey Ctrl-G: confirm it doesn't collide for target users (zsh `^G` is
  `list-expand`/unused-ish; readline `^G` is abort — acceptable since the widget is explicit).
- Phase-2 items intentionally absent: daemon protocol, agent-loop tool schema, async widget.
