# Validation log — adversarial review of design claims

Status of every load-bearing claim in `architecture.md` / `specification.md`.
Evidence tiers: **[E]** empirical (measured on this machine, see [empirical-jsonbench.md](empirical-jsonbench.md)) ·
**[M]** man page on this machine · **[W3-0]** adversarial web verification, unanimous 3-vote ·
**[W]** single-pass web research against primary docs (not adversarially re-verified) ·
**[U]** unverified, flagged.

The 2026-07-08 deep-research workflow was cut short by a session rate limit (43/106 agents);
remaining claims were validated locally — empirical and man-page evidence supersedes web votes.

## JSON crates

| Claim | Verdict | Evidence |
|---|---|---|
| miniserde has `json::Value` (Null/Bool/Number/String/Array/Object) | **confirmed** | [W3-0] + [E] round-trip incl. `Option<Value>` field, `Value` serialize-embed |
| Prior dismissal "miniserde has no Value flexibility" | **refuted** | same |
| miniserde derive: braced named-field structs + C-style enums only; no data-carrying enums | **confirmed** (unit enums *do* work; data enums don't — not needed for our shapes) | [W2-0] + [E] |
| miniserde: exactly one attribute, `rename` | **confirmed** | [W3-0] + [E] |
| miniserde ignores unknown fields; `null` → `None`; escaping correct | **confirmed** | [E] |
| Size/perf/deps: −33 KB, ~30% slower parse (irrelevant vs network), 2 vs 4 runtime deps | **measured** | [E] |
| miniserde non-recursive (stack-safe) parse/drop | **plausible, docs-stated** | [U] — verification votes rate-limited; not load-bearing |

## HTTP/TLS transport

| Claim | Verdict | Evidence |
|---|---|---|
| ureq 3 is blocking, no tokio; zero async crates in tree | **confirmed** | [E] `cargo tree`: 0 hits for tokio/async/futures/mio/hyper |
| ureq 3 default TLS = rustls 0.23 + **ring** (not aws-lc-rs); webpki-roots; no OpenSSL/cmake | **confirmed** | [E] dep tree |
| `body_mut().as_reader()` streams incrementally (unchunks; no full-body buffering) | **confirmed** | [E] drip test: first byte 957 ms, total 3 s |
| Real HTTPS works with compiled-in roots only | **confirmed** | [E] fetched example.com, no OS cert store |
| Full-stack stripped binary ~1.2 MB; cold start ~2 ms (macOS arm64) | **measured** | [E] 1,235,440 B (miniserde) / 1,268,400 B (serde_json); 2.09 ms avg |
| musl static ~2–3.5 MB (estimate) | **measured better: 1,642,776 B** aarch64-musl, fully static (`ldd`: not a dynamic program) | [E] rust:1-alpine container, rustc 1.96.1 |
| macOS full-static impossible (libSystem always dynamic) | **confirmed** | Apple platform policy, long-established; [W] |

## Shell integration

| Claim | Verdict | Evidence |
|---|---|---|
| zsh `print -z` pushes args onto the editing buffer stack | **confirmed** | [M] zshbuiltins, verbatim |
| ZLE widgets: `zle -N`/`bindkey`, `$BUFFER` writable, `$CURSOR` clamped to buffer | **confirmed** | [M] zshzle |
| bash `bind -x`: sets `READLINE_LINE`/`READLINE_POINT`/`READLINE_MARK`; changes reflected in editing state | **confirmed** | [M] bash 5.3 man |
| bash has no `print -z` equivalent outside `bind -x` | **confirmed** | [M] (absence in bash builtins) |
| macOS `/bin/bash` = 3.2.57 → **no READLINE_LINE**; widget requires bash ≥ 4.0 (MacPorts 5.3 present) | **confirmed** | [M]+[E] versions checked |
| `command_not_found_handle` invoked only when PATH/function/builtin search fails (127 path) | **confirmed** | [M] bash 5.3 man |
| `command_not_found_handle` runs **in a subshell** → cannot modify parent state/buffer | **confirmed** — spec consequence: it may only print/log | [M] "separate execution environment" |
| bash cannot name a command `?` (invalid identifier) → `?`/`??`/`???` become `ai`/`aifix`/`aiask` | **confirmed** | [E] bash 5.3: alias + function both rejected |
| fish `commandline`/`-r`/`-C` (read/replace/cursor) | **confirmed** | [W3] fish docs, S14 |
| fish `fish_command_not_found` is a function receiving command tokens | **confirmed** | [W3] fish docs |
| fish function names allow only `[A-Za-z0-9_]` → `?` impossible → `ai`/`aifix`/`aiask` | **confirmed** | [W3] fish docs |
| fish `$CMD_DURATION` gives last-command ms (used for the session log) | **confirmed** | [W3] fish docs |
| fish **`$status` is the command's exit inside `--on-event fish_postexec`**; `$argv[1]` = command line; `$CMD_DURATION` = ms | **confirmed** | [E] fish 4.8.0, real event dispatch on a tmux PTY: `EVENT status=1 cmd=[false]`, `dur=156` for `sleep 0.15`; plus the `fish_glue_…` driver test (direct-call path) |

## Provider wire protocols (single-pass vs primary docs; re-verification was rate-limited)

| Claim | Verdict | Evidence |
|---|---|---|
| OpenAI Chat Completions: `data:` SSE lines, `[DONE]` sentinel, `choices[0].delta.content`, tool-call args as chunked JSON string; `max_tokens` vs `max_completion_tokens` drift | **single-pass** | [W] developers.openai.com |
| Anthropic Messages: named SSE events `message_start → content_block_* → message_delta → message_stop`, `text_delta`/`input_json_delta`, `x-api-key` + `anthropic-version: 2023-06-01`, `max_tokens` required, no `[DONE]` | **single-pass** | [W] platform.claude.com |
| Anthropic's OpenAI-compat shim is lossy/testing-only (no prompt caching, structured outputs, extended thinking) | **single-pass** | [W] platform.claude.com/docs/en/api/openai-sdk |
| OpenAI-compat coverage: Ollama :11434/v1 · LM Studio :1234/v1 · vLLM :8000/v1 · llama.cpp :8080/v1 · OpenRouter openrouter.ai/api/v1 · NVIDIA NIM integrate.api.nvidia.com/v1 | **single-pass** | [W] each vendor's docs |
| NVIDIA-NeMo/Switchyard: Python+Rust LLM proxy translating OpenAI/Anthropic/Responses via neutral IR; a `base_url` target, not a dependency | **confirmed** | [W] repo + architecture.md fetched directly, two independent fetches |

## v0.1.0 acceptance record (spec §9, run 2026-07-08)

| # | Criterion | Status |
|---|---|---|
| 1 | `suggest` streams + prints one command; config-only provider switch | ✅ live: Vultr (openai-compat, incl. reasoning model); mock: anthropic-native headers/body asserted. Ollama/LM Studio not installed — same wire as Vultr, config-swap covered by tests; live spot-check deferred until a runner exists |
| 2 | Wire adapters pass recorded-fixture tests | ✅ `tests/fixtures/openai-vultr-nemotron.sse` = real recorded vLLM stream (24.8 KB, reasoning deltas) replayed through the full binary; anthropic docs-derived fixture likewise |
| 3 | Ctrl-G round-trip in zsh/bash/fish; `?`-family works; nothing executed | ✅ driver tests in real zsh 5.9 / bash 5.3 / fish 4.8 (buffer replace, empty→fix, no-clobber); live daily use in zsh |
| 4 | `??` fixes from state file; exit 5 on stale/absent | ✅ integration tests + live (`gti status` → `git status`, 1.8 s) |
| 5 | Planted `sk-…` never reaches the request body | ✅ asserted on the captured HTTP body (history, rerun output, piped stdin) |
| 6 | §8 budgets CI-enforced on both platforms | ✅ macOS 1,354,112 B / ~2 ms; musl aarch64 1.64 MB + x86_64 1.87 MB, both fully static (`+crt-static` build size-identical; x86_64 binary executed on glibc Debian — impossible if dynamic); budget 2.0 MB |
| 7 | Emitted glue greps clean of `eval` | ✅ zsh/bash/fish all 0 occurrences (tests + manual sweep) |

## Open items

~~1. musl binary size~~ — **closed**: 1.57 MB measured (above), budget tightened to 2.0 MB.
~~2. Provider SSE shapes~~ — **closed for the openai wire**: `tests/fixtures/openai-vultr-*.sse`
is real recorded vLLM wire data (reasoning deltas included) replayed through the full binary.
Anthropic remains docs-derived (`anthropic-docs-derived.sse`) until a live Anthropic key exists —
the sole remaining `[W]`-grade item, non-blocking.
