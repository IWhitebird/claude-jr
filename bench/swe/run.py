# SWE-bench Verified with claude-jr: for each task, run the agent inside the
# task's official Docker image, collect its patch, and score it with the
# official harness. One task at a time, with the container capped, and each
# image deleted when its task is done, so it is safe to leave running.
#
# Setup (once):
#   python3 -m venv ~/.cache/claude-jr-evals/swe-venv
#   ~/.cache/claude-jr-evals/swe-venv/bin/pip install swebench
#   CC_x86_64_unknown_linux_musl=gcc cargo build --release --target x86_64-unknown-linux-musl
#
# Run (PY = ~/.cache/claude-jr-evals/swe-venv/bin/python):
#   $PY bench/swe/run.py --dry-run                       # 1 task against bench/mock.py, costs nothing
#   $PY bench/swe/run.py --gold --n 1                    # score the reference fix: must say "resolved"
#   $PY bench/swe/run.py --recheck DIR                   # after a run: which failures aren't the agent's
#   $PY bench/swe/run.py --n 5 --budget 5                # a pilot
#   $PY bench/swe/run.py --n 50 --budget 60 --model claude-sonnet-5-5
#   $PY bench/swe/run.py --out bench/results/swe-...     # continue a run that stopped
#   for i in 0 1 2; do $PY bench/swe/run.py --n 500 --shard $i/3 --out DIR & done   # three at once
#
# The key comes from ANTHROPIC_API_KEY or ~/.config/claude-jr/credentials.

import argparse, json, os, random, shutil, socket, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BINARY = os.path.join(ROOT, "target/x86_64-unknown-linux-musl/release/claudejr")
DATASET = "SWE-bench/SWE-bench_Verified"

PROMPT = """You are in the repository {repo} at /testbed. Fix the issue below by changing the \
source code. Hidden tests will check the fix, so don't change or add tests.

Work like this:
1. Reproduce the problem first, with a short script or an existing test. If the issue includes \
an example, run that example.
2. Find every place the behaviour comes from (other classes, backends, layers), not only the \
first one that shows the symptom, and fix the issue completely. Don't make unrelated changes.
3. Run your reproduction again, then the existing tests for the code you changed. If a test \
fails, work out whether your change caused it. A failing test you can't edit is still a signal: \
don't dismiss it as outdated unless you can say exactly why the new behaviour is right.

<issue>
{issue}
</issue>"""

# Runs inside the task image, in the task's conda environment. The patch is
# what the agent changed, new files included: the difference from a snapshot
# taken just before it starts (the images come with a few edits of their own,
# such as pinned versions in setup.py, which must not be in the patch).
INSIDE = r"""
source /opt/miniconda3/bin/activate 2>/dev/null && conda activate testbed 2>/dev/null
cd /testbed
git config --global --add safe.directory /testbed
git add -A >/dev/null 2>&1
before=$(git write-tree)
start=$(date +%s)
timeout -k 10 {timeout} claudejr -y {effort} -p "$(cat /out/prompt.txt)" >/out/transcript.txt 2>&1
echo $? >/out/exit_code
echo $(( $(date +%s) - start )) >/out/seconds
git add -A >/dev/null 2>&1
git -c core.fileMode=false diff --cached "$before" >/out/patch.diff 2>/dev/null
chmod -R a+rwX /out
"""


def sh(*cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def free_gb(path="/"):
    return shutil.disk_usage(path).free / 1e9


def available_mb():
    for line in open("/proc/meminfo"):
        if line.startswith("MemAvailable"):
            return int(line.split()[1]) // 1024
    return 0


def api_key():
    key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
    if key:
        return key
    config = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    try:
        return open(os.path.join(config, "claude-jr/credentials")).read().strip()
    except OSError:
        return ""


def run_agent(task, out, args, env):
    os.makedirs(out, exist_ok=True)
    open(os.path.join(out, "prompt.txt"), "w").write(
        PROMPT.format(repo=task["repo"], issue=task["problem_statement"]))
    script = INSIDE.format(timeout=args.timeout,
                           effort=f"-e {args.effort}" if args.effort else "")
    name = "cjr-swe-" + task["instance_id"].replace("__", "-")
    sh("docker", "rm", "-f", name)
    # The key travels in the environment, never on a command line.
    cmd = ["docker", "run", "--rm", "--name", name, "--network", "host",
           "--cpus", str(args.cpus), "--memory", args.memory, "--memory-swap", args.memory, "--pids-limit", "1024",
           "-e", "ANTHROPIC_API_KEY", "-e", "ANTHROPIC_BASE_URL", "-e", "CLAUDE_JR_MODEL",
           "-e", "CLAUDE_JR_REPORT=/out/report.json", "-e", "XDG_DATA_HOME=/tmp/cj", "-e", "XDG_CONFIG_HOME=/tmp/cj",
           "-v", f"{BINARY}:/usr/local/bin/claudejr:ro", "-v", f"{os.path.abspath(out)}:/out",
           task["image"], "bash", "-c", script]
    try:
        subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=args.timeout + 120)
    except subprocess.TimeoutExpired:
        sh("docker", "rm", "-f", name)

    def read(file, default=""):
        try:
            return open(os.path.join(out, file)).read()
        except OSError:
            return default
    try:
        report = json.loads(read("report.json", "{}"))
    except ValueError:
        report = {}
    return {"patch": read("patch.diff"), "report": report,
            "exit_code": read("exit_code").strip(), "seconds": int(read("seconds", "0").strip() or 0)}


def evaluate(task, patch, out, run_id, model):
    """The official harness on one task: True if the hidden tests pass, False
    if they don't, None if the harness produced no verdict."""
    pred = os.path.join(out, "prediction.jsonl")
    open(pred, "w").write(json.dumps(
        {"instance_id": task["instance_id"], "model_name_or_path": model, "model_patch": patch}) + "\n")
    r = sh(sys.executable, "-m", "swebench.harness.run_evaluation", "--dataset_name", DATASET,
           "--predictions_path", pred, "--instance_ids", task["instance_id"], "--max_workers", "1",
           "--run_id", run_id, "--report_dir", out, cwd=out)
    open(os.path.join(out, "eval.log"), "w").write(r.stdout + r.stderr)
    for root, _, files in os.walk(out):
        for f in files:
            if f == "report.json" and "logs" in root:
                try:
                    return bool(json.load(open(os.path.join(root, f)))[task["instance_id"]]["resolved"])
                except (ValueError, KeyError):
                    pass
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=50, help="how many tasks (a fixed random sample)")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--ids", help="comma-separated instance ids instead of a sample")
    ap.add_argument("--model", default="claude-sonnet-5-5")
    ap.add_argument("--effort", default="")
    ap.add_argument("--budget", type=float, default=5.0, help="stop starting tasks once this many dollars are spent")
    ap.add_argument("--timeout", type=int, default=1500, help="seconds per task for the agent")
    ap.add_argument("--cpus", default="2")
    ap.add_argument("--memory", default="4g")
    ap.add_argument("--min-free-gb", type=float, default=20)
    ap.add_argument("--keep-images", action="store_true")
    ap.add_argument("--no-eval", action="store_true")
    ap.add_argument("--gold", action="store_true", help="score the reference fix instead of running the agent")
    ap.add_argument("--recheck", metavar="DIR", help="score the reference fix for every unresolved task of a "
                    "finished run: where that fails too, the task can't be scored on this machine")
    ap.add_argument("--dry-run", action="store_true", help="one task against bench/mock.py: tests the plumbing")
    ap.add_argument("--shard", default="", help="i/n: this process takes every n-th task, starting at i (run n of them at once)")
    ap.add_argument("--out")
    args = ap.parse_args()

    if not os.access(BINARY, os.X_OK):
        sys.exit("build the static binary first (see the top of this file)")
    from datasets import load_dataset
    tasks = {t["instance_id"]: t for t in load_dataset(DATASET, split="test")}
    if args.recheck:
        rows = [json.loads(line) for line in open(os.path.join(args.recheck, "results.jsonl"))]
        ids = [r["instance_id"] for r in rows if r["resolved"] is False]
        args.gold, args.out, args.shard = True, os.path.join(args.recheck, "reference-check"), ""
    elif args.ids:
        ids = args.ids.split(",")
    else:
        # A fixed shuffle, so a bigger --n continues the same list.
        ids = sorted(tasks)
        random.Random(args.seed).shuffle(ids)
        ids = ids[:1 if args.dry_run else args.n]
    if args.shard:
        i, n = map(int, args.shard.split("/"))
        ids = ids[i::n]

    env = dict(os.environ, CLAUDE_JR_MODEL=args.model)
    mock = None
    if args.dry_run:
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        mock = subprocess.Popen([sys.executable, "-I", os.path.join(ROOT, "bench/mock.py"), str(port)],
                                env=dict(os.environ, MOCK_ROUNDS="1"))
        time.sleep(0.6)
        env.update(ANTHROPIC_API_KEY="mock", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}")
    elif not args.gold:
        env["ANTHROPIC_API_KEY"] = api_key()
        if not env["ANTHROPIC_API_KEY"]:
            sys.exit("no API key: run claudejr and type /login, or set ANTHROPIC_API_KEY")

    out = os.path.abspath(args.out or os.path.join(ROOT, "bench/results", time.strftime("swe-%Y%m%d-%H%M%S")))
    os.makedirs(out, exist_ok=True)
    results_path = os.path.join(out, "results.jsonl")
    done = {}
    if os.path.exists(results_path):
        done = {r["instance_id"]: r for r in map(json.loads, open(results_path))}
    spent = sum(r.get("cost_usd", 0) for r in done.values())
    run_id = os.path.basename(out)
    print(f"{len(ids)} tasks, model {args.model}, budget ${args.budget:.2f}, results in {out}")

    try:
        for i, iid in enumerate(ids, 1):
            if iid in done:
                continue
            # Other shards write to the same file: the budget is for all of them.
            if os.path.exists(results_path):
                spent = sum(json.loads(line).get("cost_usd", 0) for line in open(results_path))
            if spent >= args.budget:
                print(f"budget reached (${spent:.2f}); stopping")
                break
            if free_gb() < args.min_free_gb:
                print(f"only {free_gb():.0f} GB of disk free; stopping")
                break
            while available_mb() < 2500:  # the laptop is busy: wait rather than push it into swap
                print(f"only {available_mb()} MB of memory available; waiting a minute")
                time.sleep(60)
            task = tasks[iid]
            had_image = sh("docker", "image", "inspect", task["image"]).returncode == 0
            if not had_image and sh("docker", "pull", "-q", task["image"]).returncode != 0:
                print(f"[{i}/{len(ids)}] {iid}: can't pull {task['image']}; skipped")
                continue
            task_out = os.path.join(out, iid)
            if args.gold:
                os.makedirs(task_out, exist_ok=True)
                r = {"patch": task["patch"], "report": {}, "exit_code": "", "seconds": 0}
            else:
                r = run_agent(task, task_out, args, env)
            resolved = None
            if not args.no_eval and r["patch"].strip():
                resolved = evaluate(task, r["patch"], task_out, run_id, args.model)
            elif not r["patch"].strip():
                resolved = False
            if not had_image and not args.keep_images:
                sh("docker", "rmi", task["image"])
            cost = float(r["report"].get("cost_usd") or 0)
            spent += cost
            row = {"instance_id": iid, "resolved": resolved, "cost_usd": cost, "seconds": r["seconds"],
                   "exit_code": r["exit_code"], "patch_bytes": len(r["patch"]),
                   "requests": r["report"].get("requests"), "output_tokens": r["report"].get("output_tokens"),
                   "input_tokens": r["report"].get("input_tokens"),
                   "cache_read_tokens": r["report"].get("cache_read_tokens")}
            with open(results_path, "a") as f:
                f.write(json.dumps(row) + "\n")
            with open(os.path.join(out, "predictions.jsonl"), "a") as f:
                f.write(json.dumps({"instance_id": iid, "model_name_or_path": args.model,
                                    "model_patch": r["patch"]}) + "\n")
            done[iid] = row
            mark = {True: "resolved", False: "not resolved", None: "not scored"}[resolved]
            print(f"[{i}/{len(ids)}] {iid}: {mark}, ${cost:.2f}, {r['seconds']}s, "
                  f"patch {len(r['patch'])} bytes (total ${spent:.2f})", flush=True)
    finally:
        if mock:
            mock.kill()

    # With shards, other processes add to the same file: count everything in it.
    done = {r["instance_id"]: r for r in map(json.loads, open(results_path))} if os.path.exists(results_path) else done
    spent = sum(r.get("cost_usd", 0) for r in done.values())
    scored = [r for r in done.values() if r["resolved"] is not None]
    solved = sum(1 for r in scored if r["resolved"])
    summary = {"model": args.model, "tasks_run": len(done), "scored": len(scored), "resolved": solved,
               "resolved_pct": round(100 * solved / len(scored), 1) if scored else None,
               "cost_usd": round(spent, 2), "cost_per_task": round(spent / len(done), 3) if done else None}
    json.dump(summary, open(os.path.join(out, "summary.json"), "w"), indent=2)
    print(json.dumps(summary))
    if args.recheck:
        broken = sorted(i for i, r in done.items() if r["resolved"] is not True)
        json.dump(broken, open(os.path.join(args.recheck, "not_scorable.json"), "w"), indent=2)
        print(f"{len(broken)} of {len(done)} unresolved tasks fail with the reference fix too: {broken}")


if __name__ == "__main__":
    main()
