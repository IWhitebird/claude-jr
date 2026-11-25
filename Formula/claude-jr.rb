# Homebrew formula for claude-jr: installs the released binary.
#
#   brew tap iwhitebird/claude-jr https://github.com/IWhitebird/claude-jr
#   brew install iwhitebird/claude-jr/claude-jr
#
# Written by scripts/formula.sh at each release; change that, not this.
class ClaudeJr < Formula
  desc "Small, fast coding agent for the terminal, in one binary"
  homepage "https://claudejr.iwhitebird.com"
  version "0.3.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.0/claudejr-macos-aarch64"
      sha256 "b1c779afa3c51345e2d52da3b0e987e4543ae5f2494bbaf8cce5a2ce894fae58"
    end
    on_intel do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.0/claudejr-macos-x86_64"
      sha256 "978402e1b76475966dfc95b551dd49f9d2df9470301e9dbfddb08cbd53d5b25b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.0/claudejr-linux-aarch64"
      sha256 "ee8856e8fdf1f82336570a9ba55ce95bd6ee656601417bce69a465ef16fddae5"
    end
    on_intel do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.3.0/claudejr-linux-x86_64"
      sha256 "772bfea17497466fd49a003cbf4d58904935e398f6f6551430142050ae21dd03"
    end
  end

  def install
    bin.install Dir["claudejr-*"].first => "claudejr"
  end

  test do
    assert_equal "claude-jr #{version}", shell_output("#{bin}/claudejr --version").strip
  end
end
