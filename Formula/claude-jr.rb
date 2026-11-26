# Homebrew formula for claude-jr: installs the released binary.
#
#   brew tap iwhitebird/claude-jr https://github.com/IWhitebird/claude-jr
#   brew install iwhitebird/claude-jr/claude-jr
#
# Written by scripts/formula.sh at each release; change that, not this.
class ClaudeJr < Formula
  desc "Small, fast coding agent for the terminal, in one binary"
  homepage "https://claudejr.iwhitebird.com"
  version "0.3.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.1/claudejr-macos-aarch64"
      sha256 "45fd2cf3f035c554807fe3b96d4db1d28828301e3583e3841149d0408972654c"
    end
    on_intel do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.1/claudejr-macos-x86_64"
      sha256 "f3fbf524efc89d8d3ce6be123f7c40a6fbe1a32285e0d673e235449b3a0ec1b7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.1/claudejr-linux-aarch64"
      sha256 "633abcc3b43c4da1f7133effc99bbe0db959825aeafb74b39ae4f0e86bfa9152"
    end
    on_intel do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.1/claudejr-linux-x86_64"
      sha256 "56871b0f93fc2f92edff061855bee11193f01034f3c1b802ecffa33cc066051a"
    end
  end

  def install
    bin.install Dir["claudejr-*"].first => "claudejr"
  end

  test do
    assert_equal "claude-jr #{version}", shell_output("#{bin}/claudejr --version").strip
  end
end
