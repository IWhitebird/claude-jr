#!/usr/bin/env bash
# Peak memory of a long scripted session against bench/mock.py.
# Fails if it goes over the budget.
#
#   bench/mem.sh                     # 60 rounds, 40 KB of output each
#   ROUNDS=200 OUTPUT_KB=20 bench/mem.sh

set -euo pipefail
cd "$(dirname "$0")/.."
LIMIT_KB=${LIMIT_KB:-16384}
ROUNDS=${ROUNDS:-60}
OUTPUT_KB=${OUTPUT_KB:-40}

cargo build --release -q
bin=${BIN:-$PWD/target/release/claudejr}
work=$(mktemp -d)
port=$((20000 + RANDOM % 20000))

MOCK_ROUNDS=$ROUNDS MOCK_OUTPUT_KB=$OUTPUT_KB python3 -I bench/mock.py "$port" &
mock=$!
trap 'kill $mock 2>/dev/null; rm -rf "$work"' EXIT
sleep 0.5

cd "$work"
env ANTHROPIC_API_KEY=mock ANTHROPIC_BASE_URL="http://127.0.0.1:$port" \
    XDG_DATA_HOME="$work" XDG_CONFIG_HOME="$work" \
    /usr/bin/time -f '%M %e' -o time "$bin" -y -p "run the scenario" >out 2>err \
    || { echo "run failed:"; tail -5 err; exit 1; }
read -r peak secs <time
history_kb=$(( $(wc -c <"$(find . -name '*.jsonl' | head -1)") / 1024 ))

echo "rounds:       $ROUNDS (${OUTPUT_KB} KB of tool output each)"
echo "history:      ${history_kb} KB"
echo "time:         ${secs}s"
echo "peak RSS:     ${peak} KB (limit ${LIMIT_KB} KB)"
[ "$peak" -le "$LIMIT_KB" ] && echo "ok" || { echo "over budget"; exit 1; }
