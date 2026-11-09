#!/usr/bin/env bash
# CPU and memory of claude-jr itself, against bench/mock.py (no API key).
#
#   1. idle: the interactive UI started in a pseudo-terminal, then left alone
#   2. busy: a scripted session with ROUNDS tool calls, run with -p
#
# The mock answers instantly, so "busy" measures claude-jr's own work:
# parsing streams, building requests, running tools, saving the session.
#
#   bench/cpu.sh
#   ROUNDS=200 bench/cpu.sh
#   BIN=target/x86_64-unknown-linux-musl/release/claude-jr bench/cpu.sh

set -euo pipefail
cd "$(dirname "$0")/.."
ROUNDS=${ROUNDS:-60}
OUTPUT_KB=${OUTPUT_KB:-20}
if [ -z "${BIN:-}" ]; then
    cargo build --release -q
    BIN=target/release/claude-jr
fi
bin=$(realpath "$BIN")

echo "== idle ($bin)"
python3 -I bench/idle.py "$bin" 5

work=$(mktemp -d)
port=$((20000 + RANDOM % 20000))
MOCK_ROUNDS=$ROUNDS MOCK_OUTPUT_KB=$OUTPUT_KB python3 -I bench/mock.py "$port" &
mock=$!
trap 'kill $mock 2>/dev/null; rm -rf "$work"' EXIT
sleep 0.5

cd "$work"
env ANTHROPIC_API_KEY=mock ANTHROPIC_BASE_URL="http://127.0.0.1:$port" XDG_DATA_HOME="$work" \
    /usr/bin/time -f '%e %U %S %M %c %w' -o time "$bin" -y -p "run the scenario" >/dev/null 2>&1
read -r wall user sys peak forced voluntary <time

cpu=$(awk "BEGIN { print $user + $sys }")
echo
echo "== busy: $ROUNDS tool rounds, ${OUTPUT_KB} KB of output each"
echo "wall time         ${wall}s"
echo "CPU time          ${cpu}s (user ${user}s, system ${sys}s)"
echo "CPU per round     $(awk "BEGIN { printf \"%.2f\", $cpu * 1000 / $ROUNDS }") ms"
echo "peak memory       $(awk "BEGIN { printf \"%.1f\", $peak / 1024 }") MB"
echo "context switches  $voluntary voluntary, $forced forced"
