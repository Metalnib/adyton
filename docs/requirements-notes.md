# adyton — requirements (brainstorm capture)

Historical working notes from the design discussion. Superseded by
[architecture.md](architecture.md) + [specification.md](specification.md); kept as decision trail.

## Late additions (2026-07-08 pm)
- **Name: adyton** (ἄδυτον, Delphi's inner sanctum) — user choice from Greek-mythology shortlist.
- **No curl, hard constraint** — everything in the binary → in-process rustls (killed Odin).
- **No async; threads OK** — verified already true: zero async crates in tree.
- **JSON: miniserde** — user lean, confirmed by measurement (−33 KB, −3 crates) with two binding
  mitigations (isolation in `wire/`, raw-line-in-error). See research/empirical-jsonbench.md.
- **DRY design** — codified as architecture decision D7.

## Hard priorities (ranked)
1. **Minimal binary size** — top priority.
2. **Fast startup** — <100ms for everything except the model network call (network/inference latency is inherent).
3. Static linking (musl on Linux; libSystem-only on macOS — full-static impossible on macOS).

## Scope decisions
- **Daemon: post-MVP.** MVP uses a file cache in `~/.cache` for context.
- **Async: optional, phase 2.** MVP is synchronous/blocking (one-shot).
- **Language: Rust (settled).** Constraint "everything in the binary — no external curl" mandates embedded streaming TLS; Odin has no in-process streaming-TLS path, so it's out.
- **Transport: in-process** (ureq + rustls/ring + webpki-roots). No subprocess/curl for the API call. See `architecture-and-spec.md`.

## UX
- **Primary:** hotkey widget (grab line → AI → replace line in place, editable, never auto-run). Works across zsh/bash/fish.
- **Fallback:** `?` / `??` command aliases (bash has no `print -z`, so the widget is primary there).
- **`??` context: richer than just last command + error.** Want to send more context: cwd, recent command history (N), exit codes, git status, sysinfo/OS, maybe env (redacted).
- **Overlay indicator** ("nice to have"): show what's happening / stream the LLM's thinking as it works (progress/spinner/streamed tokens).

## Providers (key requirement)
- **NOT Claude-specific.** Must support **OpenAI API and Anthropic API**.
- Model: we **forward the request to a configurable endpoint** — point `base_url` at:
  - OpenAI, Anthropic directly, OR
  - local runners (Ollama, LM Studio), OR
  - a router/gateway (NVIDIA NeMo / "Switchyard", OpenRouter, LiteLLM, vLLM).
- Provider abstraction: minimal set of wire adapters (likely OpenAI-compatible + Anthropic Messages).

## Safety
- Never auto-execute; always print to editable buffer.
- Redact secrets before sending context (env vars, keys in history).
- Prompt-injection surface if command output/files feed the loop.
