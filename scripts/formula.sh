#!/bin/sh
# Writes the Homebrew formula for a release, from that release's checksums:
#
#   scripts/formula.sh 0.2.0 checksums.txt > Formula/claude-jr.rb
#
# The release workflow runs it once the release is out, so that
# `brew install claude-jr` and `brew upgrade` get the new version.
set -eu
version=${1:?usage: scripts/formula.sh VERSION CHECKSUMS}
sums=${2:?usage: scripts/formula.sh VERSION CHECKSUMS}
url=https://github.com/IWhitebird/claude-jr/releases/download/v$version

sum() {
    found=$(grep "  claudejr-$1\$" "$sums" | cut -d' ' -f1)
    [ ${#found} = 64 ] || { echo "no checksum for claudejr-$1 in $sums" >&2; exit 1; }
    echo "$found"
}
macos_arm=$(sum macos-aarch64)
macos_intel=$(sum macos-x86_64)
linux_arm=$(sum linux-aarch64)
linux_intel=$(sum linux-x86_64)

cat <<END
# Homebrew formula for claude-jr: installs the released binary.
#
#   brew tap iwhitebird/claude-jr https://github.com/IWhitebird/claude-jr
#   brew install claude-jr
#
# Written by scripts/formula.sh at each release; change that, not this.
class ClaudeJr < Formula
  desc "Small, fast coding agent for the terminal, in one binary"
  homepage "https://claudejr.iwhitebird.com"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$url/claudejr-macos-aarch64"
      sha256 "$macos_arm"
    end
    on_intel do
      url "$url/claudejr-macos-x86_64"
      sha256 "$macos_intel"
    end
  end

  on_linux do
    on_arm do
      url "$url/claudejr-linux-aarch64"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$url/claudejr-linux-x86_64"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install Dir["claudejr-*"].first => "claudejr"
  end

  test do
    assert_equal "claude-jr #{version}", shell_output("#{bin}/claudejr --version").strip
  end
end
END
