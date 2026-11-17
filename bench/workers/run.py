# Ten independent pieces of work, done three ways by the same main agent, to
# see what handing work out buys: wall time, money, memory. Two sizes: ten
# one-line fixes (the default; small enough that handing them out only adds
# overhead), and with --each N ten packages of N fixes each.
#
#   alone      the main agent does all ten itself, one after another
#   subagents  it gives each to a subagent of its own (threads in its process)
#   workers    it gives each to a claude-jr worker over MCP, as another agent
#              would (separate processes, each in its own worktree)
#
# The main agent is claude-jr on MAIN; subagents and workers run on the
# worker model (WORKER, Haiku unless set). Each arrangement starts from the
# same repository (see fixture.py) and is scored by running the tests after.
#
#   python3 bench/workers/run.py BINARY                     all three, real API (under a dollar)
#   python3 bench/workers/run.py BINARY --each 6            the bigger job: ten packages of six fixes (a few dollars)
#   python3 bench/workers/run.py BINARY --only workers --tasks 2 --main claude-haiku-5-5   a cheap trial
#   python3 bench/workers/run.py BINARY --dry-run           against the fake API: checks the plumbing only
#
# The key is the environment's, else the one claude-jr has from /login; it is never printed.
# Results: bench/results/workers-DATE.json and a table on stdout.

import argparse, json, os, shutil, subprocess, sys, tempfile, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

WHAT = ("This repository has a small Python package: modules in pkg/, each with tests in tests/ "
        "(python3 -m unittest tests.test_NAME). {n} modules have a bug that makes their tests fail: {names}. "
        "Fix each module so that its tests pass. Do not change the tests.")
WHAT_MANY = ("This repository has {n} small Python packages, each in a folder of its own: {names}. In each folder, "
             "modules are in pkg/ and their tests in tests/ (run from inside the folder: python3 -m unittest "
             "discover -s tests -t .). Every module in every folder has a bug that makes its tests fail, and the "
             "folders do not all hold the same modules. Fix every module so that all tests pass. Do not change the tests.")
HOW = {
    "alone": "Do all of it yourself, one {unit} after another. Do not use the agent tool.",
    "subagents": ("Give each {unit} to a subagent of its own with the agent tool, with worktree set to true, and "
                  "start them all in one message so they run side by side. Tell each one everything it needs: the "
                  "{unit}, its test command, and not to touch anything else. When they have finished, merge every "
                  "branch they report, then run all the tests and fix whatever still fails yourself."),
    "workers": ("Give each {unit} to a claude-jr worker of its own with the start_task tool: one call per {unit}, "
                "all in one message, each prompt self-contained (the {unit}, its test command, and not to touch "
                "anything else). Then call wait_tasks until all have finished, merge_task each one, run all the "
                "tests and fix whatever still fails yourself. Do not fix anything yourself before that."),
}


def api_key():
    """The key from the environment, else the one claude-jr has from /login."""
    key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
    if key:
        return key
    config = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    try:
        return open(os.path.join(config, "claude-jr/credentials")).read().strip()
    except OSError:
        return ""


def memory_of(binary):
    """(processes running `binary`, the memory they hold in kB), shared pages counted once."""
    count, kb = 0, 0
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            if os.readlink(f"/proc/{pid}/exe") != binary:
                continue
            for line in open(f"/proc/{pid}/smaps_rollup"):
                if line.startswith("Pss:"):
                    kb += int(line.split()[1])
            count += 1
        except OSError:
            pass
    return count, kb


def run(arrangement, binary, names, args, env):
    """One arrangement in a fresh copy of the repository. Returns its measurements."""
    work = tempfile.mkdtemp(prefix=f"cjr-workers-{arrangement}-")
    repo, home = os.path.join(work, "repo"), os.path.join(work, "home")
    plan = fixture.services(len(names), args.each) if args.each else None
    if plan:
        fixture.write_services(repo, plan)
    else:
        fixture.write(repo, names)
    os.makedirs(os.path.join(home, "claude-jr"))
    # A copy of the binary of this run's own: its processes are then found by path.
    exe = os.path.join(work, "claudejr")
    shutil.copy2(binary, exe)
    config = {"worker_model": args.worker} if args.worker else {}
    if arrangement == "workers":
        config["mcp_servers"] = {"claude-jr": {"command": exe, "args": ["mcp", "-y"]}}
    json.dump(config, open(os.path.join(home, "claude-jr", "config.json"), "w"))
    what, pieces, unit = (WHAT_MANY, list(plan), "folder") if plan else (WHAT, names, "module")
    prompt = what.format(n=len(pieces), names=", ".join(pieces)) + " " + HOW[arrangement].format(unit=unit)
    env = dict(env, HOME=home, XDG_CONFIG_HOME=home, XDG_DATA_HOME=home)

    peak = {"kb": 0, "processes": 0}
    stop = threading.Event()

    def watch():
        while not stop.is_set():
            count, kb = memory_of(exe)
            peak["kb"], peak["processes"] = max(peak["kb"], kb), max(peak["processes"], count)
            stop.wait(0.2)

    watcher = threading.Thread(target=watch)
    watcher.start()
    start = time.time()
    try:
        done = subprocess.run([exe, "-y", "-p", prompt, "-m", args.main, "--output-format", "json",
                               "--log", os.path.join(work, "log.jsonl")],
                              cwd=repo, env=env, capture_output=True, text=True, timeout=args.timeout)
        out, err = done.stdout, done.stderr
    except subprocess.TimeoutExpired as e:
        out, err = "", f"timed out after {args.timeout}s: {(e.stderr or b'')[-300:]}"
    seconds = time.time() - start
    stop.set()
    watcher.join()
    try:
        report = json.loads(out)
    except ValueError:
        report = {"ok": False, "error": (err or out)[-400:]}

    # Workers are processes of their own: what they spent is in their results.
    tasks_dir = os.path.join(home, "claude-jr", "tasks")
    workers = []
    for task in sorted(os.listdir(tasks_dir)) if os.path.isdir(tasks_dir) else []:
        result = os.path.join(tasks_dir, task, "result.json")
        if os.path.exists(result):
            workers.append(json.load(open(result)))
    steps = [json.loads(l) for l in open(os.path.join(work, "log.jsonl"))] if os.path.exists(
        os.path.join(work, "log.jsonl")) else []
    handed = sum(1 for s in steps if s.get("type") == "step" and s.get("tool") in ("agent", "mcp__claude-jr__start_task"))
    if plan:
        fixed = sum(len(fixture.passing(os.path.join(repo, folder), mods)) for folder, mods in plan.items())
    else:
        fixed = len(fixture.passing(repo, names))
    row = {
        "arrangement": arrangement,
        "ok": bool(report.get("ok")),
        "fixed": fixed,
        "of": len(names) * (args.each or 1),
        "seconds": round(seconds, 1),
        "cost_usd": round(report.get("cost_usd", 0) + sum(w.get("cost_usd", 0) for w in workers), 4),
        "main_cost_usd": round(report.get("cost_usd", 0), 4),
        "requests": report.get("requests", 0) + sum(w.get("requests", 0) for w in workers),
        "handed_out": handed,
        "peak_memory_mb": round(peak["kb"] / 1024, 1),
        "peak_processes": peak["processes"],
        "error": report.get("error"),
    }
    if args.keep:
        row["folder"] = work
    else:
        subprocess.run(["git", "worktree", "prune"], cwd=repo, capture_output=True)
        shutil.rmtree(work, ignore_errors=True)
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("binary")
    ap.add_argument("--only", action="append", choices=list(HOW), help="run just this arrangement (repeatable)")
    ap.add_argument("--tasks", type=int, default=len(fixture.MODULES), help="how many pieces of work (up to ten)")
    ap.add_argument("--each", type=int, default=0, help="make each piece a package of this many fixes (up to ten)")
    ap.add_argument("--main", default="claude-opus-5-5", help="the main agent's model")
    ap.add_argument("--worker", help="the worker model (default: claude-jr's own default, Haiku)")
    ap.add_argument("--timeout", type=int, default=1800)
    ap.add_argument("--keep", action="store_true", help="keep each run's folder")
    ap.add_argument("--dry-run", action="store_true", help="the fake API: no key, no cost, nothing really fixed")
    args = ap.parse_args()
    binary = os.path.realpath(args.binary)
    names = list(fixture.MODULES)[:args.tasks]
    env = {"PATH": os.environ["PATH"], "CLAUDE_JR_OFFLINE_SECS": "60"}
    mock = None
    if args.dry_run:
        mock = subprocess.Popen([sys.executable, "-I", os.path.join(ROOT, "bench/mock.py"), "0"],
                                stdout=subprocess.PIPE, text=True, env=dict(os.environ, MOCK_ROUNDS="1"))
        env.update(ANTHROPIC_API_KEY="mock", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{mock.stdout.readline().strip()}")
    else:
        env["ANTHROPIC_API_KEY"] = api_key()
        if not env["ANTHROPIC_API_KEY"]:
            sys.exit("no API key: run claudejr and type /login, or set ANTHROPIC_API_KEY")
    rows = []
    try:
        for arrangement in args.only or list(HOW):
            print(f"{arrangement}: running...", file=sys.stderr, flush=True)
            rows.append(run(arrangement, binary, names, args, env))
            print(f"{arrangement}: {json.dumps(rows[-1])}", file=sys.stderr, flush=True)
    finally:
        if mock:
            mock.kill()
    print(f"{'':<10} {'fixed':>6} {'time':>8} {'cost':>8} {'peak memory':>12} {'processes':>10} {'handed out':>11}")
    for r in rows:
        print(f"{r['arrangement']:<10} {str(r['fixed']) + '/' + str(r['of']):>6} {r['seconds']:>7.0f}s "
              f"{'$%.3f' % r['cost_usd']:>8} {'%.1f MB' % r['peak_memory_mb']:>12} {r['peak_processes']:>10} "
              f"{r['handed_out']:>11}")
    if not args.dry_run:
        out = os.path.join(ROOT, "bench/results", time.strftime("workers-%Y%m%d-%H%M%S.json"))
        os.makedirs(os.path.dirname(out), exist_ok=True)
        json.dump({"main": args.main, "worker": args.worker or "default", "tasks": len(names), "each": args.each or 1,
                   "runs": rows},
                  open(out, "w"), indent=1)
        print(f"saved {out}")


if __name__ == "__main__":
    main()
