# Empirical: miniserde vs serde_json + transport validation

Measured 2026-07-08, macOS arm64, rustc 1.96.0, release profile `opt-level="z"`, `lto=true`,
`codegen-units=1`, `panic="abort"`, `strip=true`. Source: session scratchpad `jsonbench/`
(two twin binaries, identical logic, ureq 3 + rustls kept live via a real HTTPS code path).

## Round 2 — full realistic wire models + performance

Models: complete OpenAI `chat.completion.chunk` (id/object/created/model/system_fingerprint,
choices with role/content/`tool_calls[]` deltas, `Option<Value>` logprobs, `usage`), the complete
Anthropic event superset (`message_start` with nested message, `content_block_start/delta/stop`,
`message_delta`, `message_stop`, `error`), and a full request (system+messages+tools with a
JSON-schema `Value`). 200k-iteration parse loops over a realistic sample mix; 100k serialize loop.

| | miniserde 0.1.45 | serde_json 1.0.150 | delta |
|---|---|---|---|
| Stripped binary (full stack) | **1,235,440 B** | 1,268,400 B | **miniserde −32,960 B (−2.6%)** |
| OpenAI chunk parse | 0.99 µs/op · 285 MB/s | 0.77 µs/op · 368 MB/s | serde_json ~29% faster |
| Anthropic event parse | 0.33 µs/op · 325 MB/s | 0.24 µs/op · 437 MB/s | serde_json ~34% faster |
| Request serialize | 1.06 µs/op · 491 MB/s | 0.82 µs/op · 634 MB/s | serde_json ~29% faster |
| All 12 wire-shape samples | ✅ parse | ✅ parse | tie |
| Incremental clean build | 4.1 s | 4.5 s | tie |
| Runtime deps | itoa, zmij (2) | itoa, memchr, serde_core, zmij (4) | miniserde −2 (**−3 crates project-wide: 35 vs 38**) |
| Compile-time deps | mini-internal → syn/quote/proc-macro2 | serde_derive → syn/quote/proc-macro2 | tie (both pull syn) |
| Parse-error diagnostics | opaque (no position/detail) | line/column/message | serde_json |

Perf context: either crate parses an entire 1000-chunk stream in ~1 ms — 1000× below network
latency. Performance does not decide this choice; size and diagnostics do.

**Decision: miniserde** (user's minimal-size lean holds), conditional on two spec-level mitigations:
1. JSON confined to one adapter module → swap-back to serde_json is mechanical if diagnostics ever hurt.
2. Parse wrapper embeds the raw SSE line in every error — recovers ~all practical diagnostic value.

## Round 3 — ablation: what each stack actually costs

No-network variants (ureq removed) + a baseline (std + sample data only) isolate the layers.
Linkage verified via `otool -L`: only `libSystem` + `libiconv` dynamic (mandatory macOS OS libs);
all crates statically linked. musl target would be fully static incl. libc.

| Build | Size | Layer cost |
|---|---|---|
| baseline (std floor + data) | 285,936 B | — |
| + miniserde | 352,864 B | miniserde stack **+67 KB** |
| + serde_json | 385,808 B | serde stack **+100 KB** |
| + ureq/rustls/ring/webpki | 1,235,440 / 1,268,400 B | transport **+883 KB** (~72% of binary) |

Cross-check: isolated JSON delta 32,944 B vs full-stack delta 32,960 B — additive to within
16 B, so neither build accidentally stripped differently. serde's bloat reputation stems from
monomorphization at scale (hundreds of derives); at our ~15 structs the gap is 33 KB and would
widen with type count — consistent with choosing miniserde.

## Round 1 — capability verification (miniserde) — all pass

- Unknown fields (`id`, `object`, `created`, …) silently ignored ✅
- `"finish_reason": null` → `Option::None` ✅ · `#[serde(rename = "type")]` ✅
- Anthropic variant deltas as all-optional structs — **no data-carrying enums needed** ✅
- `miniserde::json::Value` exists and round-trips ✅ (`Option<Value>` fields and `Value`
  serialize-embedding verified in round 2) — the earlier "no Value flexibility" dismissal was **wrong**
- Serialization escaping (quotes, newlines) correct ✅
- Startup, 50-run avg incl. full parse workload: **2.09 ms** ✅ (~1.9 ms claim confirmed)

Known miniserde limits (by design): opaque errors; no `#[serde(default)]`, untagged/data enums,
borrowed strings; always ignores unknown fields; non-recursive (stack-safe) parsing.

## Transport validation (both rounds)

- ureq 3 default TLS = **rustls 0.23 + ring 0.17 + webpki-roots 1.0** — no aws-lc-sys, no OpenSSL,
  no cmake in tree ✅
- **Zero async crates** in the 42-crate full tree (no tokio/futures/mio/hyper) — stack is fully
  synchronous already; threads-only concurrency costs nothing extra ✅
- Real TLS round-trip: fetched `https://example.com` (559 B) with no OS cert store ✅
- **Incremental streaming proven:** against a drip endpoint (200 B over 2 s, httpbingo.org),
  `body_mut().as_reader()` returned the **first byte at 957 ms, total at 3 s** — bytes are
  delivered as they arrive, not buffered ✅ (httpbin.org itself 504'd — flaky, not a client issue)
