#!/usr/bin/env bash
# The same unattended run as night.sh, several tasks at once: most of a
# task's time is downloading its image, and the downloads overlap.
#   setsid nohup bench/swe/parallel.sh claude-opus-5-5 500 70 3 >/dev/null 2>&1 &
# Arguments: model, tasks, total budget in dollars, how many at once.
# Running it again continues where it stopped.
cd "$(dirname "$0")/../.." || exit 1
model=${1:-claude-opus-5-5}
n=${2:-500}
budget=${3:-70}
jobs=${4:-3}
py=$HOME/.cache/claude-jr-evals/swe-venv/bin/python
out=bench/results/swe-${model#claude-}
set -a
[ -f .env ] && . ./.env
set +a
mkdir -p "$out"
rm -f bench/results/swe-finished
# The budget is shared: each process re-reads the results file before a task,
# so the total can overshoot by the tasks in flight at most.
for i in $(seq 0 $((jobs - 1))); do
    $py bench/swe/run.py --n "$n" --budget "$budget" --model "$model" --shard "$i/$jobs" --out "$out" \
        >>"$out/run-$i.log" 2>&1 &
done
wait
touch bench/results/swe-finished
