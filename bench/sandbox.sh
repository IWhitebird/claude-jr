#!/usr/bin/env bash
# Checks that --sandbox keeps the agent inside the project directory.
#
# A fake model (bench/mock.py) tries to delete a file outside the project,
# write next to it, and write into $HOME. All three must fail, and a write
# inside the project must work.

set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q
bin=$PWD/target/release/claudejr

outside=$(mktemp -d -p /var/tmp)       # visible inside the sandbox, read-only
project=$(mktemp -d)
data=$(mktemp -d)
port=$((20000 + RANDOM % 20000))
echo "do not delete" >"$outside/keep.txt"
marker=".claude-jr-sandbox-test-$$"

cmd="rm -f $outside/keep.txt; echo x > $outside/new.txt; touch \$HOME/$marker; echo ok > inside.txt"
MOCK_ROUNDS=1 MOCK_CMD="$cmd" python3 -I bench/mock.py "$port" &
mock=$!
trap 'kill $mock 2>/dev/null; rm -rf "$outside" "$project" "$data" "$HOME/$marker"' EXIT
sleep 0.5

(cd "$project" && ANTHROPIC_API_KEY=mock ANTHROPIC_BASE_URL="http://127.0.0.1:$port" XDG_DATA_HOME="$data" \
    "$bin" --sandbox -y -p "try to escape" >/dev/null 2>&1) || true

fail=0
check() { if eval "$2"; then echo "  ok    $1"; else echo "  FAIL  $1"; fail=1; fi; }
echo "sandbox check:"
check "file outside the project still exists" "[ -f '$outside/keep.txt' ]"
check "no new file outside the project"       "[ ! -e '$outside/new.txt' ]"
check "nothing written to \$HOME"             "[ ! -e '$HOME/$marker' ]"
check "writes inside the project work"        "[ -f '$project/inside.txt' ]"
exit $fail
