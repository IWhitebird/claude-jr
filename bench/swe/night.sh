#!/usr/bin/env bash
# An unattended run, one model after another on the same task list. Each
# argument is model:tasks:budget. Start it detached:
#   setsid nohup bench/swe/night.sh claude-sonnet-5-5:100:40 claude-opus-5-5:50:45 >/dev/null 2>&1 &
# Progress is in bench/results/swe-<model>/run.log; bench/results/swe-finished
# appears when everything is done. Running it again continues where it stopped.
cd "$(dirname "$0")/../.." || exit 1
py=$HOME/.cache/claude-jr-evals/swe-venv/bin/python
set -a
[ -f .env ] && . ./.env
set +a
rm -f bench/results/swe-finished
for spec in "$@"; do
    IFS=: read -r model n budget <<<"$spec"
    out=bench/results/swe-${model#claude-}
    mkdir -p "$out"
    $py bench/swe/run.py --n "$n" --budget "$budget" --model "$model" --out "$out" >>"$out/run.log" 2>&1
done
touch bench/results/swe-finished
