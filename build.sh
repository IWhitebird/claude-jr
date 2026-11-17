#!/bin/sh
# The release build: the smallest binary we can make. On top of the release
# profile it rebuilds Rust's standard library for size (the stock one is built
# for speed and carries backtrace support we never use) and leaves out the
# stack-unwinding tables, which a program that aborts on panic can't use.
# That needs a nightly compiler, pinned here so a release can be rebuilt.
#
#   ./build.sh                              for this machine
#   ./build.sh x86_64-unknown-linux-musl    static Linux build; where musl-gcc
#                                           is missing: CC_x86_64_unknown_linux_musl=gcc
#   ./build.sh x86_64-pc-windows-msvc       on Windows, from Git Bash
#
# The binary lands in target/<target>/release/claudejr (.exe on Windows). Tests and everyday
# builds use plain stable cargo; see AGENTS.md.
set -e
nightly=nightly-2026-11-13
target=${1:-$(rustc -vV | sed -n 's/^host: //p')}
rustup toolchain install "$nightly" --profile minimal --component rust-src --target "$target" >/dev/null
# Windows needs its unwind tables (the system walks them), and its C runtime
# is linked in so the one file runs on a machine with nothing installed.
case $target in
    *windows*) flags="-Ctarget-feature=+crt-static" ;;
    *) flags="-Cforce-unwind-tables=no" ;;
esac
RUSTFLAGS="-Zlocation-detail=none $flags $RUSTFLAGS" \
  cargo "+$nightly" build --release --target "$target" \
  -Zbuild-std=std,panic_abort -Zbuild-std-features=optimize_for_size
ls -l "target/$target/release/"claudejr*
