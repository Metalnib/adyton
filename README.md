# Adyton

**Natural language → shell command, streamed to your prompt — never executed for you.**

Adyton (ἄδυτον — the innermost sanctum of the temple at Delphi) is a single static binary that turns plain English into shell commands, fixes your failed
commands, and answers questions — with your machine's real context (OS, installed tools, recent
commands, terminal output) so the answers fit *your* system, not a generic one.

- **One binary, ~1.4 MB, ~2 ms cold start.** No runtime, no daemon, no OpenSSL — pure-Rust TLS
  with the CA roots compiled in. Fully static on Linux (musl).
- **Any provider.** OpenAI-compatible endpoints (OpenAI, Ollama, LM Studio, vLLM, llama.cpp,
  OpenRouter, NVIDIA NIM, routers like NeMo Switchyard) and the native Anthropic API — switch
  with one config line.
- **Never auto-executes.** Commands land on your prompt for review; you press Enter.
- **Secrets stay home.** API keys live in the macOS Keychain, redaction scrubs history and
  terminal output before anything leaves the machine.

```
~ ❯ ? find the five largest files under this directory        # you type this…
~ ❯ find . -type f -exec stat -f "%z %N" {} + | sort -nr | head -5   # …and get this, editable
```

---

## Install

**One line** (macOS / Linux, prebuilt static binary — verifies its checksum):

```sh
curl -fsSL https://raw.githubusercontent.com/Metalnib/adyton/main/install.sh | sh
```

Installs to `~/.local/bin` (override with `ADYTON_INSTALL_DIR`); pin a version with
`ADYTON_VERSION=v0.1.1`. It prints the shell-integration line to add next.

**Keep it current:**

```sh
adyton selfupdate          # --check to only look; skipped for package-manager installs
```

**Package managers** (once published — see [docs/RELEASING.md](docs/RELEASING.md)):

```sh
brew install Metalnib/tap/adyton     # Homebrew (macOS/Linux) — brew upgrade to update
sudo port install adyton             # MacPorts (macOS) — sudo port upgrade adyton to update
```

**From source** (Rust ≥ 1.96):

```sh
git clone https://github.com/Metalnib/adyton && cd adyton
cargo build --release && install -m 755 target/release/adyton ~/.local/bin/
```

Licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option.

## Set up your shell

Every install method just puts the binary on your `PATH` — none edit your rc for you (the
installer and `brew` print this exact line; here it is regardless of how you installed). Add it
once to enable the **Ctrl-G** hotkey, the `?`/`??`/`???` commands, and the hooks that record your
recent commands + failures for context (stored locally `0600`, redacted before any use):

```sh
# ~/.zshrc
eval "$(adyton init zsh)"

# ~/.bashrc         (widget needs bash ≥ 4; macOS /bin/bash 3.2 gets commands + hooks only)
eval "$(adyton init bash)"

# ~/.config/fish/config.fish
adyton init fish | source
```

Optional but recommended — without it, Adyton still works as a plain CLI (`adyton suggest -- …`,
`adyton ask …`, `adyton fix`), you just lose the hotkey, the shortcut commands, and the automatic
context.

## Configure a provider

```sh
# an OpenAI-compatible endpoint (works for OpenAI, OpenRouter, Vultr, NIM, vLLM, …)
adyton config set profile.main.wire openai
adyton config set profile.main.base_url https://api.openai.com/v1
adyton config set profile.main.model gpt-4o
adyton config set default_profile main

# store the API key in the macOS Keychain — key read from stdin, never argv/history
pbpaste | adyton config set-key main        # …or run it bare and paste at the prompt

adyton config check                          # validates everything; prints the key SOURCE, never the key
```

More provider recipes:

```ini
# ~/.config/adyton/config

[profile.local]                 # Ollama — no key needed
wire = openai
base_url = http://localhost:11434/v1
model = qwen3:8b

[profile.claude]                # Anthropic, native API (base_url excludes /v1)
wire = anthropic
base_url = https://api.anthropic.com
model = claude-sonnet-4-20250514

[profile.router]                # OpenRouter (extra headers supported)
wire = openai
base_url = https://openrouter.ai/api/v1
model = qwen/qwen3-coder
extra_headers = HTTP-Referer: https://yoursite.example
```

Switch per-invocation with `--profile <name>` or permanently via `default_profile`.

> **Reasoning models** (Nemotron, DeepSeek-R1-style, thinking-mode Qwen): the default
> `max_tokens` is 4096, but heavy thinking can still eat the whole budget before any answer
> appears — give them headroom: `adyton config set profile.main.max_tokens 16384`. Adyton also shows
> the model's thinking in the overlay as it streams (💭, **experimental**) and warns when a reply is
> cut off.
>
> **Experimental:** to make a model **skip reasoning server-side** (where the provider supports it),
> set `extra_body` — a JSON object merged into the request. There's no universal switch; it's
> per-provider: `{"reasoning_effort":"none"}` (OpenAI), `{"chat_template_kwargs":{"enable_thinking":false}}`
> (Qwen3/vLLM). Many hosted gateways strip unknown fields, so verify with `config check` and a test
> call. To just *hide* thinking locally, use `show_thinking = false` / `--no-thinking` instead.

## Use it

### The hotkey: Ctrl-G (all shells, in-place)

- Type a request in plain English → **Ctrl-G** → the line is *replaced* by the command,
  cursor at the end, nothing executed.
- Press **Ctrl-G on an empty line** right after a command failed → the corrected command appears.

### Quick commands

| zsh | bash / fish | does |
|---|---|---|
| `? <words>` | `ai <words>` | natural language → command, pushed to your prompt |
| `??` | `aifix` | fix the last **failed** command |
| `??? <words>` | `aiask <words>` | ask a question → prose answer, streamed to the terminal |

```sh
? resize all pngs in this folder to 50%
??                                        # after `gti status` → `git status` on your prompt
??? why is my docker build slow
```

(zsh gets the `?` forms because only zsh can name commands that way — bash and fish reject `?`
as an identifier, so they use the `ai*` names. The hotkey is identical everywhere.)

### Pipe anything in as context

The killer move for "explain **this**" — pipe real output straight into the question:

```sh
lsof -nP -iTCP -sTCP:LISTEN | ??? which of these is OrbStack
docker logs api 2>&1 | tail -50 | ??? why did it crash
kubectl get pods | ? delete all the crashloops
```

Piped input is redacted, capped (8 KB tail), and marked as untrusted data in the prompt.
It survives `--no-context` — you piped it on purpose.

### Full command reference

```
adyton suggest [OPTS] -- <query>     natural language → command on stdout
adyton ask     [OPTS] -- <question>  prose answer, streamed
adyton fix     [OPTS] [--rerun]      correct the last failed command
adyton init    <zsh|bash|fish>       print shell glue for eval
adyton selfupdate [--check] [--yes]  update to the latest GitHub release
adyton context refresh               rebuild the machine-facts cache now
adyton config  get <key> | set <key> <value> | set-key <profile> | check [-p <p>] | path
adyton --version | --help

OPTS: -s/--shell <zsh|bash|fish>  -p/--profile <name>  --no-context  --plain  --no-thinking  --api-key <key>
```

`fix --rerun` re-executes the failed command (your explicit consent — it may have side effects)
and attaches its output, redacted and capped, so the model sees the actual error.

Exit codes: `0` ok · `1` provider/HTTP error · `2` usage · `3` config · `4` API key · `5` no/stale
failure state for `fix`.

## What the model sees (and doesn't)

Adyton assembles context per request — every tier bounded, and **everything user-generated passes
the redaction ruleset first** (API-key shapes like `sk-…`/`ghp_…`/`AKIA…`, `password=`/`token:`
values, long hex/base64 blobs; invocations of `security`/`pass`/`op`/`gpg` are dropped whole):

| Context | Source | Notes |
|---|---|---|
| machine facts | cached (`~/.cache/adyton/context`, 24 h, refreshed in the background) | OS, arch, shell, installed package managers & top-level packages, ~40 common tools — so it suggests `rg` because you *have* it, and BSD flags because you're on macOS |
| cwd, git | live, timeboxed | branch, dirty, last commit subject |
| recent commands | session log written by the glue | command + exit code + duration, last 20 |
| terminal scrollback | **auto** in tmux, GNU screen, Zellij, WezTerm, kitty | via each terminal's own permission-free CLI; iTerm2/Terminal.app have none — use the pipe |
| piped stdin | `cmd \| ???` | explicit, survives `--no-context` |

`--no-context` sends your query alone. The API key is held in memory only, zeroized after use,
and never appears on any process's argv. Nothing is ever auto-executed — captured output is
framed as untrusted data, so even a hostile `README` printing "run rm -rf" can at worst produce
a suggestion you'd still have to press Enter on.

## Config reference

`~/.config/adyton/config` — flat `key = value` with `[profile.<name>]` sections (`adyton config
path` shows the location; written `0600`).

| Key | Default | Meaning |
|---|---|---|
| `default_profile` | — | profile used without `--profile` (a sole profile is auto-selected) |
| `timeout_seconds` | 60 | whole-request ceiling |
| `session_log_commands` | 20 | recent commands sent (0 disables) |
| `scrollback_lines` | 120 | terminal capture depth (0 disables) |
| `git_timeout_ms` | 200 | git probe budget, skipped on miss |
| `show_thinking` | true | **experimental** — stream the model's reasoning to the overlay (`--no-thinking` overrides) |

Per profile: `wire` (`openai`/`anthropic`) · `base_url` · `model` · `max_tokens` (4096) ·
`temperature` · `token_param` (`max_tokens`/`max_completion_tokens`) · `extra_body` (**experimental**
— a JSON object shallow-merged into the request; see below) · `extra_headers`
(repeatable `Name: value`) · `api_key_cmd` (argv-style command printing the key — what
`set-key` wires to the Keychain) · `api_key` (discouraged; prefer the Keychain or
`ADYTON_API_KEY`/`ADYTON_<PROFILE>_API_KEY` env vars).

Key resolution order: `--api-key` → `ADYTON_API_KEY` → `ADYTON_<PROFILE>_API_KEY` →
`api_key_cmd` → `api_key`. Keyless profiles are fine for local endpoints.

## Development

```sh
cargo test                                     # ~160 unit + integration tests, no network needed
cargo clippy --all-targets -- -D warnings      # pedantic, clean
scripts/ci/size-gate.sh target/release/adyton 1572864     # budgets are CI-enforced
```

Design docs: [architecture](docs/architecture.md) (decisions D1–D7) ·
[specification](docs/specification.md) (contracts, budgets, acceptance criteria) ·
[validation log](docs/research/validation-log.md) (every load-bearing claim, with evidence).
Dependencies are deliberately three: `ureq` (rustls/ring), `miniserde`, `lexopt` — enforced by a
CI gate. Wire fixtures under `tests/fixtures/` include real recorded vLLM streams.
