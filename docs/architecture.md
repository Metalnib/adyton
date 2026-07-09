# Adyton — Architecture

**Status:** v2 (2026-07-08) · supersedes `architecture-and-spec.md` (split into this + [specification.md](specification.md))
**Evidence:** every load-bearing claim is tracked in [research/validation-log.md](research/validation-log.md); measurements in [research/empirical-jsonbench.md](research/empirical-jsonbench.md).

**Adyton** (ἄδυτον — the innermost sanctum of the Delphi temple, where the oracle's answers
were delivered) is a single self-contained binary that turns natural language into shell
commands and fixes failed ones. You ask; the answer appears on your prompt — editable, never
executed for you.

This document is the *why*. The exact contracts (CLI, config, wire formats, shell glue) live in
[specification.md](specification.md). Rationale is stated once, here; the spec does not repeat it.

---

## 1. Priorities

1. **Minimal binary size** — drives every dependency choice.
2. **Fast startup** — <100 ms for all local work (target <20 ms); the model's network latency is
   inherent and excluded from this budget. Perceived speed comes from streaming tokens, not from
   shaving milliseconds off a network-bound call.
3. **Everything in the binary** — no external processes for transport (no curl), no OS trust-store
   reads, no runtime downloads. Fully static on Linux (musl); libSystem-only dynamic on macOS
   (full-static is impossible there by Apple policy).
4. **Provider-agnostic** — forward requests to any configurable endpoint.

Non-goals for MVP: daemon (phase 2), async shell UX (phase 2), inline ghost-text (phase 3),
auto-execution (never).

## 2. Decisions

Each decision links its evidence; alternatives were eliminated by measurement or hard constraint.

### D1. Language: Rust
"Everything in the binary" requires an *embedded, streaming-capable* TLS stack. Odin has none
(no TLS in core/vendor; the one community HTTP lib is beta, OpenSSL-bound, and cannot stream),
so it fails the constraint outright. Rust does it in-process with pure-Rust TLS at a measured
**1.24 MB / ~2 ms cold start** — the size goal is met with the constraint satisfied.

### D2. Transport: in-process, blocking, threads-only
- **ureq 3 + rustls/ring + webpki-roots**: no OpenSSL, no cmake, CA roots compiled in
  (zero startup I/O), incremental streaming proven by measurement (first byte mid-stream at
  957 ms on a 2 s drip).
- **No async runtime — verified, not aspirational**: the dependency tree contains zero async
  crates (no tokio/futures/mio/hyper). There is nothing left to remove; a threads-only design
  costs no extra size. Concurrency needs are trivial: the main thread blocks on the response
  stream; one `std::thread` renders the stderr spinner/token overlay; a stale context cache
  refreshes on a third thread *during* the API call — network dead time pays for the probes,
  so no request ever waits on a rebuild (atomic rename + single-flight lock make abandonment safe).
- The phase-2 daemon reuses this same transport plus a warm connection pool — no rewrite.

### D3. JSON: miniserde (isolated, swappable)
Measured head-to-head on full realistic wire models: miniserde is **−33 KB and −3 runtime
crates** (itoa+zmij vs itoa+memchr+serde_core+zmij); serde_json parses ~30% faster and has
line/column errors. Parse speed is irrelevant here (a whole 1000-chunk stream parses in ~1 ms
with either), so the trade is size vs diagnostics. We take the size, and buy the diagnostics
back cheaply — two conditions are **binding on the implementation**:
1. All JSON code lives in one module (`wire/`), so swapping back to serde_json is mechanical.
2. Every parse error wraps the raw offending line — for short SSE lines that recovers nearly
   all the diagnostic value of positional errors.

Earlier research dismissed miniserde on false grounds ("no Value"); adversarial re-verification
and empirical tests corrected this — all our wire shapes parse, including `Option<Value>` fields
and `Value`-embedded tool schemas, with no data-carrying enums needed.

### D4. Providers: two wire adapters, routers are endpoints
The minimal adapter set is **{OpenAI-compatible, Anthropic-native}**:
- OpenAI-compatible reaches OpenAI, Ollama, LM Studio, vLLM, llama.cpp, LiteLLM, OpenRouter,
  NVIDIA NIM — and gateways like **NVIDIA-NeMo/Switchyard** — via a `base_url` swap.
- Anthropic keeps a native adapter because its own OpenAI-compat shim is documented as
  lossy/testing-only (no prompt caching, structured outputs, extended thinking).
- Routers (Switchyard, LiteLLM) are *deployment targets*, not dependencies: they absorb
  cross-provider translation server-side; Adyton just points at them. We do not rebuild a router.

Both adapters normalize onto one internal event stream (`TextDelta` / `ToolCallDelta` /
`Done{stop_reason, usage}`) — the streaming wire formats genuinely diverge (flat deltas +
`[DONE]` sentinel vs typed, indexed block events), which is exactly why an adapter layer exists.

### D5. Shell UX: widget-first, alias fallback, never eval
The binary is a **pure function**: (query + context) → command string on stdout. Shell glue
decides where the string goes. Rationale for widget-first: bash has no `print -z` equivalent, and
a subprocess can't touch the parent's readline buffer — a `bind -x` widget is the *only* clean
injection path there, so the widget is the primary UX everywhere for one consistent mental model.
Output is always placed by assignment (`print -z --`, `READLINE_LINE=`, `commandline -r --`),
never `eval` — LLM output must never be parsed as shell code.

### D6. Context: bounded, cached, redacted
Richer `??` context (history, git, sysinfo) is a startup-budget spend. Slow-changing sources are
cached to disk; per-invocation sources are capped or timeboxed; obvious secrets are redacted
before anything leaves the machine.

Command *history* and command *output* are architecturally different: the shell has the former,
but output exists only in the terminal's scrollback. So history is enriched shell-side (our
prompt hooks log exit code/duration/cwd per command — far more signal than raw lines), while
output capture is **tiered by what's present**: tmux/screen pane capture → terminal remote APIs
with OSC 133 marks (phase 1.5) → consent-gated re-run. Always-on recording wrappers are rejected
(TTY fidelity, indiscriminate retention). Captured output and history are redacted and
size-capped before send, and treated as **untrusted model input** (prompt-injection surface —
delimited and system-prompt-hardened; never-auto-execute is the backstop). The phase-2 agent
loop inherits the same rule for tool outputs.

### D7. DRY as a design rule
One fact, one place: glue scripts are embedded in the binary and emitted by `init` (no separate
plugin files to drift); both adapters share the event model and one SSE reader; JSON lives in one
module; system prompts/templates are single constants; docs state rationale here and contracts in
the spec, each exactly once.

## 3. System shape

```
 ~/.zshrc · ~/.bashrc · config.fish
     eval "$(adyton init <shell>)"        ← glue embedded in the binary (D7)
                │
      shell glue: widget (primary) + ? / ?? aliases (fallback)     [D5]
      captures line or last-cmd state; places result by assignment
                │ argv/stdin ↓        ↑ stdout = command · stderr = overlay
 ┌──────────────┴───────────────────────────────┐
 │ adyton (one static binary)                   │
 │  args (lexopt) → context (cache + bounded    │
 │  live probes [D6]) → wire adapter [D4]       │
 │  → transport: ureq+rustls, blocking [D2]     │
 │  → SSE reader → event stream → formatter     │
 │  spinner thread → stderr                     │
 └──────────────────────────────────────────────┘
        │ HTTPS (streamed SSE)
   any OpenAI-compatible endpoint · Anthropic
   (incl. Ollama/LM Studio/vLLM/OpenRouter/NIM/Switchyard/LiteLLM)
```

## 4. Latency & size budgets (rationale)

The end-to-end reply is network-bound (0.3–3 s+); the local budget exists so adyton itself is
never perceptible: spawn ~1–5 ms, parse+context <20 ms, measured full-stack cold start ~2 ms.
Sub-100 ms *end-to-end* is only reachable with a local model — supported for free via
`base_url` → Ollama/llama.cpp, not a special code path. Binary budget: ≤1.5 MB macOS arm64
(measured 1.24 MB), musl target ≤3.5 MB pending CI measurement. Hard numbers in the
[spec §8](specification.md).

## 5. Phasing

- **Phase 1 (MVP):** everything above, synchronous one-shot. `suggest` + `fix` + `init` +
  `context` + `config`.
- **Phase 2:** daemon (same transport + warm pool → kills the 100–300 ms TLS handshake),
  persistent context, agent loop (tool calls for system introspection; injection-hardened per
  D6), async zsh widget, richer overlay.
- **Phase 3:** inline ghost-text (zsh `zpty`; bash lacks an async path), Responses API adapter
  if it ever becomes interop-relevant.

## 6. Security posture

Never auto-execute; never eval model output; secrets stay in process memory (no argv, no temp
files) and are zeroized after use; CA roots compiled in; context redacted before send; context
subprocesses use explicit arg vectors. Details and the redaction ruleset: [spec §7](specification.md).
