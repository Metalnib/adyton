# Homebrew formula for Adyton (S20). Source of truth lives here; the release
# workflow renders `version`, `url`s and `sha256`s from each tag and pushes a
# copy to the Metalnib/homebrew-tap repo. Install: `brew install Metalnib/tap/adyton`.
#
# The {{...}} tokens are substituted by the release workflow. The committed
# values track the current release so the formula is always a working example.
class Adyton < Formula
  desc "Natural language to shell command, streamed to your prompt (never executed)"
  homepage "https://github.com/Metalnib/adyton"
  version "{{VERSION}}"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/Metalnib/adyton/releases/download/v{{VERSION}}/adyton-v{{VERSION}}-aarch64-apple-darwin.tar.gz"
      sha256 "{{SHA256_AARCH64_APPLE_DARWIN}}"
    end
    on_intel do
      url "https://github.com/Metalnib/adyton/releases/download/v{{VERSION}}/adyton-v{{VERSION}}-x86_64-apple-darwin.tar.gz"
      sha256 "{{SHA256_X86_64_APPLE_DARWIN}}"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Metalnib/adyton/releases/download/v{{VERSION}}/adyton-v{{VERSION}}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "{{SHA256_AARCH64_UNKNOWN_LINUX_MUSL}}"
    end
    on_intel do
      url "https://github.com/Metalnib/adyton/releases/download/v{{VERSION}}/adyton-v{{VERSION}}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "{{SHA256_X86_64_UNKNOWN_LINUX_MUSL}}"
    end
  end

  def install
    bin.install "adyton"
  end

  def caveats
    <<~EOS
      Enable the shell integration by adding one line to your shell rc:
        zsh  ->  eval "$(adyton init zsh)"
        bash ->  eval "$(adyton init bash)"
        fish ->  adyton init fish | source

      Then configure a provider (see the README):
        adyton config set-key <profile>

      Update Homebrew installs with `brew upgrade` (not `adyton selfupdate`).
    EOS
  end

  test do
    assert_match "adyton #{version}", shell_output("#{bin}/adyton --version")
  end
end
