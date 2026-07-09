# adyton — Implementation stories (Phase 1 / MVP)

Derived from [specification.md](specification.md) (contracts) and [architecture.md](architecture.md)
(decisions D1–D7). Stories reference spec sections instead of restating them (D7).
Sizing: **S** ≤ half day · **M** ≈ 1 day · **L** ≈ 2–3 days.

**Global definition of done (every story):** `cargo fmt` + `clippy -D warnings` clean · unit tests
for new code · CI budget gates green (S1) · no new dependency without a decision record in
architecture.md · no `eval` in any emitted shell code.

## Milestones

```
M0 foundation   S1  S2
M1 wire         S3 → S4 → S5, S6          (parallel with M2)
M2 context      S7, S8 → S9               (parallel with M1)
M3 commands     S10 → S11                 (needs M1 + M2)
M4 shell glue   S12 → S13, S14            (needs S10)
M5 release      S15                       (needs all)
```

---

## M0 — Foundation

### S1 · Repo, crate scaffold, CI budget gates — **M**
Goal: a buildable `adyton` skeleton whose CI enforces the budgets from day one.
- `git init`; cargo scaffold with pinned deps + release profile per spec §1; module layout
  `src/{cli,config,wire,context,glue,overlay}/`.
- Vendor the jsonbench evidence app into `bench/jsonbench/` (source of the wire structs; keeps
  measurements reproducible).
- CI (GitHub Actions): build `aarch64-apple-darwin` + `x86_64-unknown-linux-musl`; gates: binary
  size (spec §8), cold-start ≤ 20 ms, forbidden-deps check (`cargo tree` grep: tokio/async/serde/
  openssl), fmt+clippy.
- **Measure musl size → tighten spec §8 budget and close validation-log open item 1.**
Acceptance: `adyton --version` runs on both targets; a deliberately fat test branch fails the size gate.

### S2 · CLI surface + config + key resolution — **M**
Goal: the §2 command tree and §3 configuration are fully parsed and testable without network.
- lexopt parsing for all subcommands/options; exit codes 0/2/3/4 per §2.1 (`1`/`5` arrive with S10/S11).
- Flat kv reader (§3): comments, `[profile.name]` sections, repeatable `extra_headers`; explicit
  "no TOML" guard tests (quoted values are literals).
- API-key chain: flag → `ADYTON_API_KEY` → `ADYTON_<PROFILE>_API_KEY` → `api_key_cmd` (argv
  exec) → file key; in-memory only, zeroized after use (D2/§10).
- `config get|set|path` (set rewrites preserving comments or appends).
Acceptance: unit tests per §2.1 exit-code row; key-chain test with all five sources present picks by precedence.

---

## M1 — Wire (parallel with M2)

### S3 · Blocking transport — **M**
Goal: §4.3 exactly — ureq client wrapped behind the `Transport` seam.
- Connect timeout 5 s, whole-request `timeout_seconds`; one retry on connection-reset before
  first byte; non-2xx → `WireError{status, body}` (body read fully, never fed to SSE parsing).
- `as_reader()` exposed as `BufRead` lines to callers.
Acceptance: tests against a local mock server (std `TcpListener` fixture, no new deps): 200-chunked,
401/429/500 with JSON bodies, mid-stream reset (retry fires only pre-first-byte).

### S4 · SSE reader + unified event model — **S**
Goal: one reader both adapters share (D4/D7).
- Line grammar: `data: <json>` accumulation, blank/comment skip, `[DONE]` sentinel, named-event
  (`event:`) capture for Anthropic; emits `TextDelta / ToolCallDelta / Done / WireError`.
- Parse-failure wrapping: `"unparseable chunk: <line>"` (D3 mitigation).
Acceptance: table-driven tests incl. torn lines across read boundaries, interleaved `ping`, empty data.

### S5 · OpenAI-compatible adapter — **M**
Goal: §4.1 request build + response mapping with the bench-proven miniserde structs.
- `token_param` switch, optional auth header (local runners), `extra_headers`, `extra_params`.
- tool-call fragment accumulation keyed by `index`; `finish_reason` → `Done`.
Acceptance: recorded-fixture tests (happy, tool-call fragments split mid-string, error event,
non-2xx) — fixtures captured live in S15; hand-written until then.

### S6 · Anthropic native adapter — **M**
Goal: §4.2 — headers, top-level `system`, required `max_tokens`, event dispatch
(`text_delta`/`input_json_delta`/`message_delta`/`message_stop`, ignore `ping`, `error`→`WireError`).
Acceptance: fixture tests mirroring S5; explicit test that **no** `[DONE]` is expected.

---

## M2 — Context (parallel with M1)

### S7 · Context cache + background refresh — **L**
Goal: §7.1 exactly, including the concurrency contract.
- Probes (argv-only, 300 ms each / 2 s total): os/kernel/arch/shell/hw, manager detect list,
  curated `tools` probe, `packages.*` per manager.
- Staleness > 24 h → serve stale + refresh thread during API call; `context.tmp` + atomic rename;
  `context.lock` single-flight (stale 60 s); first-run sync path with 2 s cap → degrade to
  os/arch/shell; version-mismatch invalidation; `adyton context refresh` foreground.
Acceptance: kill-during-refresh test leaves a valid cache; lock no-op test; stale-serve test
(mtime manipulation); degraded first-run test.

### S8 · Redaction module — **S**
Goal: §7.2 ruleset as one table in `src/context/redact.rs`.
- Patterns: kv-secret mask, key shapes (`sk-…`, `ghp_…`, `xox…`, `AKIA…`, long base64/hex),
  secret-tool line drop (`security`, `pass`, `op`, `gpg`).
Acceptance: unit test per pattern row + a negative corpus (normal commands untouched).

### S9 · Live context + prompt assembly — **M**
Goal: §7 table + §7.3/§7.4 tier 1 + the system prompt (single constant, D7).
- Session-log read (last `session_log_commands`), failure-state read, git probe with
  `git_timeout_ms` skip, tmux/screen scrollback capture (tail 8 KB), fenced untrusted delimiters,
  `--no-context`.
- Everything passes S8 redaction before assembly.
Acceptance: prompt snapshot tests (with/without tmux, git, session log); planted `sk-…` in
history provably absent from assembled prompt (pre-body half of §9 criterion 5).

---

## M3 — Commands

### S10 · `suggest` end-to-end + stderr overlay — **L** *(needs S3–S6, S9)*
Goal: §2.1 I/O contract + §6 overlay.
- Pipeline: args → context → adapter → event stream → command on stdout (exit 0/1/4).
- Overlay thread: spinner + phase, dimmed streamed tokens, `\r`/`\x1b[K` only, cleared on `Done`;
  suppressed by `--plain`/non-TTY; stops on first delta or error.
Acceptance: §9 criterion 1 (Ollama + one cloud endpoint, config-switch only); §9 criterion 5
full (request-body assertion); overlay contract test via captured stderr.

### S11 · `fix` + `--rerun` — **M** *(needs S10)*
Goal: §5.3 consumption + §7.4 tier 3.
- Exit 5 on absent/stale (>10 min) state; scrollback attach; `--rerun`: re-exec `2>&1`, 8 KB cap,
  `timeout_seconds/4`, never default.
Acceptance: §9 criterion 4; rerun cap/timeout tests with a synthetic noisy failing command.

---

## M4 — Shell glue

### S12 · Glue framework + zsh — **M** *(needs S10)*
Goal: `init` machinery (embedded strings, D7) + the reference shell.
- `adyton init zsh`: Ctrl-G widget (`zle -N`, `$BUFFER`/`$CURSOR` restore), `?`/`??` via
  `print -z --`, precmd failure capture + session-log append (§5.3/§7.3), `zshexit` cleanup;
  insert only on exit 0.
Acceptance: §9 criterion 3 (zsh) scripted via `zsh -i` in CI or expect-style harness; criterion 7
(`init` output greps clean of `eval`).

### S13 · bash glue — **M** *(needs S12)*
Goal: §5.1 bash row. `bind -x` widget (`READLINE_LINE/POINT`), ≥ 4.0 guard with clear message on
3.2, `PROMPT_COMMAND` capture, `command_not_found_handle` annotate-only (subshell constraint),
alias prints-with-hint fallback.
Acceptance: §9 criterion 3 (bash 5.x); explicit 3.2 degradation test (macOS `/bin/bash`).

### S14 · fish glue — **M** *(needs S12; opens with verification)*
Goal: close validation-log open item 2, then implement.
- **First:** install fish (MacPorts/CI) and verify the four single-pass claims (`commandline -r`,
  `bind \cg`, `fish_command_not_found`, `fish_postexec` + `$status`); update validation-log.
- Then: widget, abbreviation `?`, `fish_postexec` capture, `fish_exit` cleanup.
Acceptance: §9 criterion 3 (fish 3.x); validation-log rows upgraded from [W] to [M]/[E].

---

## M5 — Release

### S15 · Fixture recording, acceptance sweep, docs, packaging — **M** *(needs all)*
- Record live SSE fixtures (OpenAI, Anthropic, Ollama, LM Studio) → replaces hand-written S5/S6
  fixtures; **closes validation-log open item 3** (protocol claims re-verified by tests).
- Run the full §9 criteria matrix; fix fallout; tag v0.1.0; release artifacts for both targets
  (+ checksums).
- **README.md — the full user-facing manual.** Install (download / `cargo install`),
  `eval "$(adyton init <shell>)"` per shell, config quickstart + `config set-key` (keychain), and
  **every feature with a short example**: `?`/`??`/`???` aliases + Ctrl-G, `suggest`/`ask`/`fix`
  (+`--rerun`), provider profiles (OpenAI-compat + Anthropic, local runners, routers), the pipe
  (`lsof | ???`), terminal auto-capture matrix, context/redaction/keychain behavior, all config
  keys, reasoning-model `max_tokens` note.
- Acceptance: all seven §9 criteria pass; validation-log has no open items; README documents every
  shipped command, alias, flag, and config key.

---

## M6 — v0.1.1: distribution & self-update  ✅ (implemented; release is one signed tag push)

Goal: a stranger installs adyton in one line and it keeps itself current — without breaking the
size/minimalism ethos. **No `self_update` crate** (drags reqwest + dozens of deps); hand-roll on
the ureq + miniserde already in the tree, and shell out to the system `sha256` tool for integrity
(same "shell out for the rare system thing" pattern as keychain-via-`security`). Zero new deps.

### S16 · Release CI workflow — **M** *(foundation for the rest)*
`.github/workflows/release.yml`, triggered on tag `v*`. Build matrix → run gates → package →
publish, so every release is reproducible (v0.1.0 was hand-built locally — this replaces that):
- targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`.
- per target: build `--release`, run the §8 size + startup gates, `tar.gz` with the **stable name
  `adyton-<version>-<triple>.tar.gz`** (the contract install.sh + self-update depend on).
- one `SHA256SUMS.txt` across all artifacts; create the GitHub Release and upload.
Acceptance: pushing a tag yields a Release with 4 tarballs + checksums; gates enforced per target.

### S17 · `install.sh` — **M**
`curl -fsSL https://raw.githubusercontent.com/Metalnib/adyton/main/install.sh | sh`:
- detect os/arch → triple; resolve the latest (or `ADYTON_VERSION`-pinned) release via the GitHub
  API; download the matching tarball **and** `SHA256SUMS.txt` over HTTPS; **verify** with
  `shasum -a 256` / `sha256sum`; install to `${ADYTON_INSTALL_DIR:-~/.local/bin}` (warn if not on
  PATH); print the exact `eval "$(adyton init <shell>)"` line for the detected shell.
- POSIX `sh`, idempotent, no sudo; clear failure messages. Acceptance: clean-machine install in a
  fresh Linux container + macOS, ending at a working `adyton --version`.

### S18 · `adyton selfupdate` — **M**
New subcommand (`adyton selfupdate [--check] [--yes]`):
- GET the "latest release" JSON (ureq), parse the tag (miniserde), semver-compare to
  `CARGO_PKG_VERSION`; `--check` reports and exits.
- if newer: download the current-triple asset to a temp file **in the same dir as
  `current_exe()`**, verify SHA256 via the system tool, `chmod +x`, atomic `rename()` over
  `current_exe()` (Unix replaces a running binary's path safely).
- **guardrails:** refuse if `current_exe()` isn't writable, or sits under a package-manager prefix
  (`/opt/homebrew`, `/opt/local`, `/usr`, `/nix`) — tell the user to update via that manager
  instead. Never auto-updates; no background phone-home.
Acceptance: unit tests for triple mapping + version compare; integration test drives the updater
against a mock release endpoint (download → verify → swap) in a temp dir; a live `--check` against
the real GitHub release.

### S19 · docs, help, acceptance — **S**
README install one-liner + `selfupdate` section; spec §12 update contract (triple map, integrity,
guardrails); `--help` updated; validation-log note. Acceptance: README lets a stranger install and
self-update from zero.

### S20 · Homebrew tap — **S** *(fully automatable)*
Repo `Metalnib/homebrew-tap` with `Formula/adyton.rb` (downloads the release tarball for the
host arch, verifies sha256, installs the binary + a caveat printing the `init` line). The S16
release workflow bumps the formula's `version`/`url`/`sha256` and pushes to the tap on each tag.
`brew install Metalnib/tap/adyton`, `brew upgrade` for updates. Acceptance: tap install on a
clean macOS ends at a working `adyton --version`; a new tag auto-bumps the formula.

### S21 · MacPorts Portfile — **M** *(prepare + maintain; upstream publish is external)*
`contrib/macports/Portfile` using the `cargo` PortGroup, fetching the tagged GitHub source
tarball; the release workflow updates the version + checksums. Ship **local-install docs**
(`port` from a local file / private ports tree) now; provide an **upstream-submission checklist**
for the PR into `macports/macports-ports` (review + maintainership are MacPorts', not ours — this
step is manual by nature). Acceptance: `port install` from the local Portfile builds and runs;
Portfile stays version-bumped by CI. NOT in scope: `cargo-binstall`, `crates.io` publish
(deferred — revisit if Rust-ecosystem reach is wanted).

---

## M7 — v0.2.0: response quality & overlay UX  ✅ (implemented)

Goal: kill the 1024-token papercut, tell the user when a reply is clipped, surface the model's
reasoning while it streams, and ship human release notes. Zero new deps — all four ride the
existing wire event model + stderr overlay.

### S22 · max_tokens default + truncation guard — **S**
- Default `max_tokens` 1024 → **4096** (`config.rs:85`; update wire + config tests). It's a *cap*,
  not a target, so short commands cost the same — this only stops reasoning/prose being clipped.
- Thread `stop_reason` out of `stream_command` (today dropped at `run.rs:254`); on
  `length`/`max_tokens`:
  - `suggest`/`fix` (command buffered before print): **suppress** the truncated command and return
    the advice error — never insert a half-command at the prompt.
  - `ask` (streamed live): print the partial, then a stderr warning (can't un-stream).
- Advice names the fix (`adyton config set profile.<p>.max_tokens 16384`) and, if reasoning deltas
  were seen (S23), says "this is a reasoning model" explicitly.
Acceptance: a truncated `suggest` errors with advice and prints nothing on stdout; default is 4096;
README §config + spec §3 updated.

### S23 · thinking in the overlay — **M** *(ships experimental)*
- New `Event::ReasoningDelta(String)` in `wire/event.rs`.
- Anthropic: emit the `thinking_delta` currently **skipped** at `anthropic.rs:201` (extended
  thinking). OpenAI-compatible: parse `reasoning_content` / `reasoning` deltas (DeepSeek-R1,
  thinking-Qwen, vLLM, routers; OpenAI's own API streams only token counts, no reasoning text).
- Overlay (`overlay.rs`): a `reasoning()` method + a dimmed `💭 …last thoughts…` line that flips to
  the command/answer tail on the first `TextDelta`. Reasoning **never** reaches stdout — critical
  for `ask` capture (`run.rs:191`).
- On by default (TTY + non-`--plain`); `--no-thinking` flag + `show_thinking` config key to disable.
Acceptance: unit — `reasoning`/`reasoning_content` (openai) and `thinking_delta` (anthropic) parse
to `ReasoningDelta`, and `frame_line` renders the `💭` tail until the command starts; integration —
a reasoning+content stream yields only the command/answer on stdout (the overlay is inert under
piped stderr, so stdout purity is the observable property). NOTE: the Anthropic arm is parse-ready
but won't fire until the request opts into a `thinking` block (deferred); the live path is the
OpenAI/vLLM `reasoning` field.

### S24 · CHANGELOG + human release notes — **S**
`generate_release_notes: true` yields noise (we push straight to `main`, no PRs). Keep a
`CHANGELOG.md` (Keep a Changelog format, `## [x.y.z] — YYYY-MM-DD`); the `release` job extracts the
tag's section and passes it as the Release `body` (auto-notes at most a fallback). Backfill 0.1.0 +
0.1.1. Acceptance: `v0.2.0`'s Release body is the hand-written CHANGELOG section, not a commit dump.

### S25 · `extra_body` + CLI ergonomics — **S** *(added mid-M7; `extra_body` ships experimental)*
Research finding: there is **no universal API flag** to disable reasoning — it's per-model
(`reasoning_effort` OpenAI · `chat_template_kwargs.enable_thinking` Qwen3/vLLM · Anthropic off by
default · DeepSeek-R1 can't · Nemotron-Cascade-2 via chat template, which **Vultr's allowlist
strips**). So the portable lever is a per-profile **`extra_body`**: a JSON object shallow-merged into
the request body (`wire::json::merge_into`), validated by `config check` (`wire::json::is_object`) —
JSON stays in `wire/` per D3, config stores it raw. Also: a run option placed **before** the command
now returns a "goes after the command" hint, not a bare "invalid option" (`--version`/`--help` remain
the only leading flags — standard subcommand convention). Acceptance: `extra_body` reaches the
request body (wire + integration tests); `config check` rejects a non-object; the leading-option
error names the flag and the fix.

---

## Explicitly deferred (phase 2+ — not stories yet)
Daemon + warm pool · agent loop/tool calls · async zsh widget · terminal-API scrollback (tier 2b,
OSC 133) · ghost text · Responses API adapter.
