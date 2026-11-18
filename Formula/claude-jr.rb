# Homebrew formula for claude-jr: installs the released binary.
#
#   brew tap iwhitebird/claude-jr https://github.com/IWhitebird/claude-jr
#   brew install iwhitebird/claude-jr/claude-jr
#
# Written by scripts/formula.sh at each release; change that, not this.
class ClaudeJr < Formula
  desc "Small, fast coding agent for the terminal, in one binary"
  homepage "https://claudejr.iwhitebird.com"
  version "0.2.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.2.0/claudejr-macos-aarch64"
      sha256 "63e7a1edd14d92df01fc5d07e301c85b072cbad366e1b4ad2acd228654a19e14"
    end
    on_intel do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.2.0/claudejr-macos-x86_64"
      sha256 "fa9112c20afb6a3590df8df2e6d63abc20cdf01412d340c34f1d09cc8f5300c2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.2.0/claudejr-linux-aarch64"
      sha256 "99db7aedaaaa286a473827cb19caf851847e68a23f36ceab8e62ec1996058456"
    end
    on_intel do
      url "https://github.com/IWhitebird/claude-jr/releases/download/v0.2.0/claudejr-linux-x86_64"
      sha256 "af15d232a05132feb0e7b45306d1307821178d1c3e07bdf6f1cd28694d75df7f"
    end
  end

  def install
    bin.install Dir["claudejr-*"].first => "claudejr"
  end

  test do
    assert_equal "claude-jr #{version}", shell_output("#{bin}/claudejr --version").strip
  end
end
