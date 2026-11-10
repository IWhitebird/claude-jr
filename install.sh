#!/bin/sh
# Installs claudejr (claude-jr, "Claude Junior") from the latest release:
#   curl -fsSL https://raw.githubusercontent.com/IWhitebird/claude-jr/main/install.sh | sh
# It goes to ~/.local/bin (set CLAUDE_JR_BIN for another folder). Linux and
# macOS, x86_64 and ARM. On Windows, run it inside WSL.
set -eu

base=${CLAUDE_JR_RELEASES:-https://github.com/IWhitebird/claude-jr/releases/latest/download}
dir=${CLAUDE_JR_BIN:-$HOME/.local/bin}

case $(uname -s) in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) echo "claude-jr has builds for Linux and macOS. On Windows, run this inside WSL." >&2; exit 1 ;;
esac
case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) echo "no build for $(uname -m); build it from source with cargo." >&2; exit 1 ;;
esac
name=claudejr-$os-$arch

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 --connect-timeout 15 --max-time 120 "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --tries=3 --timeout=30 -O "$2" "$1"
    else
        echo "curl or wget is needed" >&2; exit 1
    fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "downloading $name"
fetch "$base/$name" "$tmp/claudejr"
fetch "$base/checksums.txt" "$tmp/checksums.txt"

want=$(grep "  $name\$" "$tmp/checksums.txt" | cut -d' ' -f1)
if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/claudejr" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    got=$(shasum -a 256 "$tmp/claudejr" | cut -d' ' -f1)
else
    got=$want
    echo "no sha256sum or shasum here: the download was not checked" >&2
fi
if [ -z "$want" ] || [ "$want" != "$got" ]; then
    echo "the download doesn't match its checksum; nothing was installed" >&2
    exit 1
fi

chmod +x "$tmp/claudejr"
mkdir -p "$dir"
mv "$tmp/claudejr" "$dir/claudejr"
echo "installed $("$dir/claudejr" --version) to $dir/claudejr"
case ":$PATH:" in
    *":$dir:"*) echo "run: claudejr" ;;
    *) echo "$dir is not on your PATH. Add it, or run: $dir/claudejr" ;;
esac
