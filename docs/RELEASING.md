# Releasing Adyton

Cutting a release is one signed tag push; the rest is automated (spec §12, stories S16–S21).

## Cut a release
1. Bump `version` in `Cargo.toml` (and let `cargo build` sync `Cargo.lock`).
2. Commit, then tag and push:
   ```sh
   git tag -a v0.1.1 -m "Adyton 0.1.1"
   git push origin main --tags
   ```
3. `.github/workflows/release.yml` builds the four targets natively, gates each on size,
   packages `adyton-v<ver>-<triple>.tar.gz`, generates `SHA256SUMS.txt`, and publishes the
   GitHub Release with auto notes.

## Homebrew tap (S20) — one-time setup, then automatic
1. Create a **public** repo `Metalnib/homebrew-tap` (empty).
2. Create a fine-grained PAT with **Contents: Read and write** scoped to `homebrew-tap`, and add it
   to the `adyton` repo as the Actions secret **`TAP_TOKEN`**.
3. Thereafter every tag push renders `contrib/homebrew/adyton.rb` from the release checksums and
   pushes `Formula/adyton.rb` to the tap. Users: `brew install Metalnib/tap/adyton`.
   (Until the secret exists the `homebrew` job no-ops — releases never block on it.)

## MacPorts (S21) — prepare in-repo, submit upstream manually
MacPorts is source-based and its ports tree is review-gated, so this is not fully automatable.
Per release, finalize `contrib/macports/Portfile`:
1. **Source checksums** — after the tag exists:
   ```sh
   port checksum <local-portfile>   # or: openssl dgst -sha256 / -rmd160, and wc -c on the
                                     # github archive tarball for v<ver>
   ```
   Fill the `{{RMD160}}` / `{{SHA256}}` / `{{SIZE}}` tokens.
2. **Crate list** — regenerate the `cargo.crates` block:
   ```sh
   port cargo2port Cargo.lock       # append its output in place of {{CARGO_CRATES}}
   ```
3. **License nesting** — confirm the dual-license form against the current MacPorts guide
   (`license {MIT Apache-2}` vs the nested OR form).
4. **Submit** a PR to `macports/macports-ports` (new port `sysutils/adyton`), take maintainership.
   Users, once accepted: `sudo port install adyton`.

## Not shipping (deferred)
`cargo-binstall` and a `crates.io` publish — revisit if Rust-ecosystem reach is wanted; both would
build on the same release artifacts.
