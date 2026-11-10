#!/usr/bin/env bash
# Pauses a run. Finished tasks are kept; tasks in flight are redone when the
# run is started again with the same command.
pkill -f "bench/swe/parallel.sh" 2>/dev/null
pkill -f "bench/swe/night.sh" 2>/dev/null
pkill -f "bench/swe/run.py" 2>/dev/null
sleep 2
docker ps --format '{{.Names}}' | grep '^cjr-swe-' | xargs -r docker rm -f >/dev/null
echo "paused. done so far: $(cat "$(dirname "$0")"/../results/swe-*/results.jsonl 2>/dev/null | wc -l) task results"
