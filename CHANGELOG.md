# Changelog

All notable changes to Adyton are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-07-09

### Fixed
- `ask` no longer confabulates about Adyton itself: its system prompt now carries
  an accurate description of what Adyton is, so "what is adyton"-style questions
  are answered from fact instead of invention.
- The `## recent commands` context is now framed as observed shell history — not
  facts to repeat or trust — matching the scrollback and piped-input framing.

## [0.2.0] - 2026-07-09

### Changed
- Default `max_tokens` raised from 1024 to 4096. It is a cap, not a target, so
  short commands cost the same — this just stops reasoning and longer answers
  being clipped by the old limit.

### Added
- **Experimental:** the streaming overlay now shows the model's reasoning in a
  live multi-line `💭` panel while it thinks, for providers that stream it
  (`reasoning` / `reasoning_content`, or Anthropic extended thinking). On by
  default; disable with `--no-thinking` or the `show_thinking = false` config key.
- Truncation handling: a command cut off at the token limit is suppressed rather
  than inserted at your prompt, and `ask` warns after its streamed partial —
  both with advice to raise `max_tokens`, naming reasoning models as such.
- **Experimental:** per-profile `extra_body` — a JSON object shallow-merged into
  the request, for provider-specific knobs (e.g. `reasoning_effort`,
  `chat_template_kwargs`). Validated by `config check`.

### Fixed
- A run option placed before its command (`adyton --plain suggest …`) now returns
  a hint to put it after the command, instead of a bare "invalid option".

## [0.1.1] - 2026-07-09

### Added
- One-line `install.sh`, an `adyton selfupdate` command, a Homebrew tap and a
  MacPorts Portfile.
- Release CI: signed-tag-driven builds for macOS (arm64 + x86_64) and Linux musl
  (arm64 + x86_64), each a checksummed static tarball.

## [0.1.0] - 2026-07-09

### Added
- Initial release. Natural language → shell command (`suggest` / `?`), fix the
  last failed command (`fix` / `??`) and answer questions (`ask` / `???`) —
  streamed to your prompt, never auto-executed. zsh/bash/fish integration,
  machine context with redaction, macOS Keychain keys, and any OpenAI-compatible
  or Anthropic endpoint.
