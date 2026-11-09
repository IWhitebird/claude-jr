#!/usr/bin/env bash
# An unattended run: a 5-task pilot, then on to 50 tasks if the pilot's cost
# per task says 50 fit in the budget. Start it detached:
#   setsid nohup bench/swe/night.sh [model] [budget] >/dev/null 2>&1 &
# Progress is in bench/results/swe-<model>/run.log; "finished" appears at the end.
cd "$(dirname "$0")/../.." || exit 1
model=${1:-claude-sonnet-5-5}
budget=${2:-60}
out=bench/results/swe-${model#claude-}
py=$HOME/.cache/claude-jr-evals/swe-venv/bin/python
set -a
[ -f .env ] && . ./.env
set +a
mkdir -p "$out"
rm -f "$out/finished"

$py bench/swe/run.py --n 5 --budget 8 --model "$model" --out "$out" >>"$out/run.log" 2>&1
per=$($py -c "import json; print(json.load(open('$out/summary.json')).get('cost_per_task') or 0)" 2>/dev/null || echo 0)
echo "pilot: \$$per per task" >>"$out/run.log"

# Go on only if 50 tasks at the pilot's price stay inside the budget.
if $py -c "import sys; sys.exit(0 if 0 < $per * 50 <= $budget else 1)"; then
    $py bench/swe/run.py --n 50 --budget "$budget" --model "$model" --out "$out" >>"$out/run.log" 2>&1
else
    echo "stopped after the pilot: 50 tasks at \$$per each don't fit in \$$budget" >>"$out/run.log"
fi
touch "$out/finished"
