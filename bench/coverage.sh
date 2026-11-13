#!/usr/bin/env bash
# Line coverage of `cargo test`, per file. The tests in tests/ run the real
# binary, and it is instrumented too, so they count.
#
#   bench/coverage.sh            # the table
#   bench/coverage.sh src/api.rs # the lines of one file that no test reaches
#
# Needs: rustup component add llvm-tools
set -euo pipefail
cd "$(dirname "$0")/.."
tools=$(dirname "$(find "$(rustc --print sysroot)" -name llvm-cov | head -1)")
[ -x "$tools/llvm-cov" ] || { echo "run: rustup component add llvm-tools"; exit 1; }
dir=$PWD/target/cov
rm -rf "$dir/prof"; mkdir -p "$dir/prof"
# Test binaries left by earlier builds would be counted as code no test ran.
[ -d "$dir/debug/deps" ] && find "$dir/debug/deps" -maxdepth 1 -type f -executable ! -name '*.so' -delete
export RUSTFLAGS="-C instrument-coverage" CARGO_TARGET_DIR=$dir LLVM_PROFILE_FILE="$dir/prof/%p-%m.profraw"
cargo test -q 2>&1 | grep -E "test result|FAILED|panicked|error" || true
"$tools/llvm-profdata" merge -sparse "$dir"/prof/*.profraw -o "$dir/cov.profdata"
objects=()
for f in "$dir"/debug/claudejr $(find "$dir/debug/deps" -maxdepth 1 -type f -executable ! -name '*.so'); do
    # llvm-cov takes the first binary bare and the others with -object.
    [ -f "$f" ] && { [ ${#objects[@]} -eq 0 ] && objects+=("$f") || objects+=(-object "$f"); }
done
ignore='/\.cargo/|/rustc/|/\.rustup/|/tests/'
if [ $# -gt 0 ]; then
    "$tools/llvm-cov" show "${objects[@]}" -instr-profile="$dir/cov.profdata" --ignore-filename-regex="$ignore" \
        "$PWD/$1" 2>/dev/null | grep -E '^ +[0-9]+\| +0\|' || echo "every line of $1 is covered"
else
    "$tools/llvm-cov" report "${objects[@]}" -instr-profile="$dir/cov.profdata" --ignore-filename-regex="$ignore" 2>/dev/null \
        | awk -v root="$PWD/" 'NR>2 && NF>10 { sub(root, "", $1); printf "%-26s %5s lines, %5s missed  %s\n", $1, $8, $9, $10 }'
fi
