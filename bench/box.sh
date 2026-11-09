#!/usr/bin/env bash
# Runs a session inside a container limited to 0.1 CPU and 64 MB of RAM and
# reports the container's peak memory (the agent plus every command it ran).
#
#   bench/box.sh                      # claude-jr against bench/mock.py
#   bench/box.sh claude-jr -p "..."   # any command; pass ANTHROPIC_API_KEY for the real API
#
# Needs docker and a static build:
#   CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl

set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE=${IMAGE:-rust:1.99.1-slim}  # anything with bash
CPUS=${CPUS:-0.1}
MEMORY=${MEMORY:-64m}
ROUNDS=${ROUNDS:-20}
OUTPUT_KB=${OUTPUT_KB:-40}
bin=$PWD/target/x86_64-unknown-linux-musl/release/claude-jr
[ -x "$bin" ] || { echo "build the static binary first (see the top of this file)"; exit 1; }

env_args=()
if [ $# -eq 0 ]; then
    port=$((20000 + RANDOM % 20000))
    MOCK_ROUNDS=$ROUNDS MOCK_OUTPUT_KB=$OUTPUT_KB python3 -I bench/mock.py "$port" &
    mock=$!
    trap 'kill $mock 2>/dev/null' EXIT
    sleep 0.5
    env_args=(-e ANTHROPIC_API_KEY=mock -e ANTHROPIC_BASE_URL="http://127.0.0.1:$port")
    set -- claude-jr -y -p "run the scenario"
else
    env_args=(-e ANTHROPIC_API_KEY)
fi

cmd=$(printf '%q ' "$@")
docker run --rm --network host --cpus "$CPUS" --memory "$MEMORY" --memory-swap "$MEMORY" \
    "${env_args[@]}" -e XDG_DATA_HOME=/tmp -v "$bin:/usr/local/bin/claude-jr:ro" "$IMAGE" \
    bash -c "cd /tmp && start=\$(date +%s.%N) && $cmd >/tmp/out 2>&1; code=\$?;
        end=\$(date +%s.%N)
        echo \"exit code:  \$code\"
        echo \"wall time:  \$(echo \"\$end - \$start\" | bc 2>/dev/null || awk \"BEGIN{print \$end - \$start}\")s\"
        echo \"peak memory: \$(( \$(cat /sys/fs/cgroup/memory.peak) / 1024 )) KB (container, limit $MEMORY, $CPUS CPU)\"
        [ \$code -eq 0 ] || tail -5 /tmp/out"
