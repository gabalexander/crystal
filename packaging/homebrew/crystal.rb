# crystal's Homebrew formula, for a tap of its own: it installs a release's
# archive for the machine, checked against its checksum. formula.sh, beside
# this file in crystal's repository, fills in a release's version, where
# its archives are and their checksums:
#
#   packaging/homebrew/formula.sh 0.4.0 > Formula/crystal.rb
#
# Homebrew's own crystal is the Crystal programming language, so this one is
# installed by its tap's name: brew install gabalexander/crystal/crystal
class Crystal < Formula
  desc "One terminal for all your coding agents"
  homepage "https://github.com/gabalexander/crystal"
  version "@VERSION@"
  license "MIT"

  on_macos do
    on_arm do
      url "@RELEASES@/download/v@VERSION@/crystal-@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256_aarch64-apple-darwin@"
    end
    on_intel do
      url "@RELEASES@/download/v@VERSION@/crystal-@VERSION@-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256_x86_64-apple-darwin@"
    end
  end

  on_linux do
    on_arm do
      url "@RELEASES@/download/v@VERSION@/crystal-@VERSION@-aarch64-unknown-linux-musl.tar.gz"
      sha256 "@SHA256_aarch64-unknown-linux-musl@"
    end
    on_intel do
      url "@RELEASES@/download/v@VERSION@/crystal-@VERSION@-x86_64-unknown-linux-musl.tar.gz"
      sha256 "@SHA256_x86_64-unknown-linux-musl@"
    end
  end

  def install
    bin.install "crystal"
    generate_completions_from_executable(bin/"crystal", "completions")
  end

  def caveats
    <<~EOS
      A crystal daemon left running goes on running the crystal it started
      from. After an upgrade, hand it over to this one, its sessions carrying
      on, and bring the skill that teaches Claude Code to drive crystal up to
      date:
        crystal restart-server
        crystal skill --install
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/crystal --version")
  end
end
