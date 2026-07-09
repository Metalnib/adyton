# jsonbench — evidence for the JSON crate decision (architecture D3)

Twin binaries, identical logic, different JSON crate: `mini-test` (miniserde) and
`serde-test` (serde_json). They parse real OpenAI `chat.completion.chunk` / Anthropic
streaming-event shapes, serialize a full request, and keep ureq 3 + rustls live via a real
HTTPS code path so full-stack binary sizes are honest.

Results and methodology: [../../docs/research/empirical-jsonbench.md](../../docs/research/empirical-jsonbench.md).

Reproduce:

```sh
cd mini-test  && cargo build --release && ls -l target/release/mini-test  && ./target/release/mini-test
cd serde-test && cargo build --release && ls -l target/release/serde-test && ./target/release/serde-test
# live incremental-streaming proof:
./mini-test/target/release/mini-test --sse "https://httpbingo.org/drip?duration=2&numbytes=200&delay=0"
```

The ablation round (baseline / JSON-only variants) is derived by removing the `--sse` block and
the `ureq` dependency — see the empirical doc for the exact numbers.
