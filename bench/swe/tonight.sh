#!/usr/bin/env bash
# One unattended night: a 20-task pilot at default and at high effort, a
# choice between them by a fixed rule, then the full run with what won, and a
# check of which failures aren't the agent's. Start it as its own service, so
# it doesn't die with the terminal it was started from (the out-of-memory
# guard kills whole terminal sessions):
#   systemd-run --user --unit cjr-swe --collect -p MemoryMax=4G \
#       -E CHOICE=default -E JOBS=2 "$PWD/bench/swe/tonight.sh"
#   systemctl --user status cjr-swe     journalctl --user -u cjr-swe
# CHOICE=default or high skips the high-effort pilot and the choosing.
# Progress: bench/results/tonight.log. bench/swe/stop.sh pauses it; running
# this again continues where it stopped.
#   MODEL (claude-opus-5-5)  TASKS (500)  CAP dollars for everything (160)
#   JOBS at once (3)  MEMORY per container (3g)
cd "$(dirname "$0")/../.." || exit 1
model=${MODEL:-claude-opus-5-5}
tasks=${TASKS:-500}
cap=${CAP:-160}
jobs=${JOBS:-3}
memory=${MEMORY:-3g}
py=$HOME/.cache/claude-jr-evals/swe-venv/bin/python
res=bench/results
log=$res/tonight.log
set -a
[ -f .env ] && . ./.env
set +a
mkdir -p "$res"
rm -f $res/swe-finished
say() { echo "$(date +%H:%M) $*" >>"$log"; }

# Memory watchdog: the runner already waits before starting a task when
# memory is short; this stops the newest task container if it gets critical.
(
    while [ ! -f $res/swe-finished ]; do
        avail=$(awk '/MemAvailable/ {print int($2 / 1024)}' /proc/meminfo)
        # Percent of the last 10 s that something stalled waiting for memory:
        # what the system's out-of-memory guard acts on.
        stall=$(sed -n 's/^some avg10=\([0-9]*\).*/\1/p' /proc/pressure/memory)
        if [ "$avail" -lt 1200 ] || [ "${stall:-0}" -ge 30 ]; then
            victim=$(docker ps --format '{{.Names}}' | grep '^cjr-swe-' | head -1)
            [ -n "$victim" ] && docker rm -f "$victim" >/dev/null 2>&1 &&
                say "memory: ${avail} MB free, pressure ${stall}%: stopped $victim"
        fi
        echo "$(date +%H:%M) ${avail} MB free, pressure ${stall:-0}%" >>$res/memory.log
        sleep 30
    done
) &

# run OUT N BUDGET [EFFORT]: N tasks into OUT, JOBS at once, sharing BUDGET.
run() {
    out=$1; n=$2; budget=$3; effort=${4:-}
    mkdir -p "$out"
    for i in $(seq 0 $((jobs - 1))); do
        $py bench/swe/run.py --n "$n" --budget "$budget" --model "$model" --memory "$memory" \
            ${effort:+--effort "$effort"} --shard "$i/$jobs" --out "$out" >>"$out/run-$i.log" 2>&1 &
        pids="$pids $!"
    done
    wait $pids
    pids=
}
field() { $py -c "import json,sys; print(json.load(open('$1/summary.json')).get('$2') or 0)" 2>/dev/null || echo 0; }

say "pilot: 20 tasks at default effort"
run $res/swe-pilot-default 20 20
if [ -z "${CHOICE:-}" ]; then
    say "pilot: the same 20 at high effort"
    run $res/swe-pilot-high 20 25 high
fi

d_solved=$(field $res/swe-pilot-default resolved); d_cost=$(field $res/swe-pilot-default cost_per_task)
h_solved=$(field $res/swe-pilot-high resolved); h_cost=$(field $res/swe-pilot-high cost_per_task)
spent=$($py -c "print(round($(field $res/swe-pilot-default cost_usd) + $(field $res/swe-pilot-high cost_usd), 2))")
left=$($py -c "print(round($cap - $spent, 2))")
say "default: $d_solved/20 at \$$d_cost a task; high: $h_solved/20 at \$$h_cost a task; \$$spent spent, \$$left left"

# High effort only if it solves at least 2 more of the 20 and all the tasks fit in what's left.
choice=${CHOICE:-default}
if [ -z "${CHOICE:-}" ] && $py -c "import sys; sys.exit(0 if $h_solved - $d_solved >= 2 and $h_cost * ($tasks - 20) <= $left else 1)"; then
    choice=high
fi
say "chosen: $choice effort"
echo "$choice" >$res/swe-choice

# The full run starts from the chosen pilot's 20 results, so they aren't paid for twice.
full=$res/swe-${model#claude-}-full
if [ ! -f "$full/results.jsonl" ]; then
    mkdir -p "$full"
    cp -r $res/swe-pilot-$choice/. "$full/"
    rm -f "$full"/run-*.log "$full/summary.json"
fi
budget=$($py -c "print(round($left + $(field $res/swe-pilot-$choice cost_usd), 2))")
say "full run: $tasks tasks, budget \$$budget"
if [ "$choice" = high ]; then run "$full" "$tasks" "$budget" high; else run "$full" "$tasks" "$budget"; fi
say "full run done: $(field "$full" resolved) of $(field "$full" scored) resolved, \$$(field "$full" cost_usd)"

say "checking which failures aren't the agent's"
$py bench/swe/run.py --recheck "$full" >>"$full/recheck.log" 2>&1
say "finished"
touch $res/swe-finished
