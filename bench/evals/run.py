#!/usr/bin/env python3
# Runs coding tasks with an agent and checks the results.
#
#   python3 bench/evals/run.py frontend                         # all 10 frontend tasks
#   python3 bench/evals/run.py frontend --tasks debounce,dark-mode
#   python3 bench/evals/run.py polyglot-python --limit 10       # Aider's polyglot benchmark
#   python3 bench/evals/run.py frontend --agent claude-code --model claude-sonnet-5-5
#   python3 bench/evals/run.py frontend --dry-run               # against bench/mock.py, free
#
# Suites:
#   frontend         bench/evals/frontend/: HTML, CSS, accessibility, JS, and one
#                    page built from a design image. Each has hidden checks.
#   polyglot-LANG    Exercism exercises from github.com/Aider-AI/polyglot-benchmark
#                    (python, rust, go), downloaded on first use. Example
#                    solutions are removed before the agent sees the folder.
#
# Every task runs in a fresh temporary copy. Spending is capped by --budget
# (dollars, default 2): a task isn't started if the average so far says it
# would go over. claude-jr and Claude Code report their cost; pi doesn't, so
# its cost shows as "?" and only the task count limits it.

import argparse, hashlib, json, os, shutil, socket, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.join(os.environ.get("XDG_CACHE_HOME", os.path.expanduser("~/.cache")), "claude-jr-evals")
POLYGLOT_REPO = "https://github.com/Aider-AI/polyglot-benchmark"

POLYGLOT_PROMPT = (
    "{instructions}\n\nUse the above instructions to modify the supplied files: {files}. Don't change the names "
    "of existing functions or classes, as they may be referenced from other code like unit tests. Only use "
    "standard libraries, don't install any packages."
)
POLYGLOT_TESTS = {
    "python": ["python3", "-m", "unittest", "discover", "-p", "*_test.py"],
    "rust": ["cargo", "test", "--quiet", "--", "--include-ignored"],
    "go": ["go", "test", "./..."],
}


# ---- tasks ------------------------------------------------------------------------

class Task:
    def __init__(self, name, prompt, setup, check, timeout=420):
        self.name, self.prompt, self.setup, self.check, self.timeout = name, prompt, setup, check, timeout


def frontend_tasks():
    tasks = []
    base = os.path.join(HERE, "frontend")
    for name in sorted(os.listdir(base)):
        d = os.path.join(base, name)
        meta = json.load(open(os.path.join(d, "task.json")))
        setup = lambda work, d=d: shutil.copytree(os.path.join(d, "start"), work, dirs_exist_ok=True)
        check = lambda work, d=d: run([sys.executable, "-I", os.path.join(d, "check.py")], work, 120)
        tasks.append(Task(name, meta["prompt"], setup, check, meta.get("timeout", 420)))
    return tasks


def polyglot_tasks(lang):
    repo = os.path.join(CACHE, "polyglot-benchmark")
    if not os.path.isdir(repo):
        os.makedirs(CACHE, exist_ok=True)
        print(f"downloading {POLYGLOT_REPO} ...", file=sys.stderr)
        subprocess.run(["git", "clone", "-q", "--depth", "1", POLYGLOT_REPO, repo], check=True)
    base = os.path.join(repo, lang, "exercises", "practice")
    tasks = []
    for name in sorted(os.listdir(base)):
        d = os.path.join(base, name)
        config = json.load(open(os.path.join(d, ".meta", "config.json")))
        solution = config["files"]["solution"]
        instructions = ""
        for doc in ("introduction.md", "instructions.md", "instructions.append.md"):
            p = os.path.join(d, ".docs", doc)
            if os.path.exists(p):
                instructions += open(p).read() + "\n"
        prompt = POLYGLOT_PROMPT.format(instructions=instructions.strip(), files=", ".join(solution))

        def setup(work, d=d):
            # Everything except .meta, which holds the example solution.
            shutil.copytree(d, work, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".meta"))

        check = lambda work: run(POLYGLOT_TESTS[lang], work, 300)
        tasks.append(Task(name, prompt, setup, check))
    return tasks


def run(cmd, cwd, timeout):
    """(passed, last lines of output)."""
    try:
        r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return r.returncode == 0, (r.stdout + r.stderr)[-1500:]
    except subprocess.TimeoutExpired:
        return False, "check timed out"


# ---- agents -----------------------------------------------------------------------

def agent_command(agent, prompt, model, work, report):
    """argv and extra env for one non-interactive run."""
    if agent == "claude-jr":
        binary = os.path.abspath(os.environ["CLAUDE_JR_BIN"]) if os.environ.get("CLAUDE_JR_BIN") else next(
            (p for p in (os.path.join(ROOT, "target/x86_64-unknown-linux-musl/release/claude-jr"),
                         os.path.join(ROOT, "target/release/claude-jr")) if os.path.exists(p)), "claude-jr")
        argv = [binary, "-y", "-p", prompt] + (["-m", model] if model else [])
        return argv, {"CLAUDE_JR_REPORT": report}
    if agent == "claude-code":
        argv = ["claude", "-p", prompt, "--dangerously-skip-permissions", "--output-format", "json"]
        return argv + (["--model", model] if model else []), {}
    if agent == "pi":
        argv = [os.path.abspath(os.environ["PI_BIN"]) if os.environ.get("PI_BIN") else "pi", "--print", prompt]
        return argv + (["--provider", "anthropic", "--model", model] if model else []), {}
    raise SystemExit(f"unknown agent {agent}")


def cost_of(agent, report, stdout):
    try:
        if agent == "claude-jr":
            return json.load(open(report))["cost_usd"]
        if agent == "claude-code":
            return json.loads(stdout)["total_cost_usd"]
    except (OSError, ValueError, KeyError):
        pass
    return None


# ---- runner -----------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="Run coding tasks with an agent and check the results.")
    ap.add_argument("suite", help="frontend, polyglot-python, polyglot-rust or polyglot-go")
    ap.add_argument("--agent", default="claude-jr", choices=["claude-jr", "claude-code", "pi"])
    ap.add_argument("--model", help="model id (default: the agent's own default)")
    ap.add_argument("--tasks", help="comma-separated task names")
    ap.add_argument("--limit", type=int, help="run at most this many tasks (spread across the suite)")
    ap.add_argument("--budget", type=float, default=2.0, help="stop before spending more than this (USD)")
    ap.add_argument("--dry-run", action="store_true", help="use bench/mock.py instead of the API (free)")
    args = ap.parse_args()

    if args.suite == "frontend":
        tasks = frontend_tasks()
    elif args.suite.startswith("polyglot-") and args.suite[9:] in POLYGLOT_TESTS:
        tasks = polyglot_tasks(args.suite[9:])
    else:
        raise SystemExit(f"unknown suite {args.suite}")
    if args.tasks:
        wanted = set(args.tasks.split(","))
        tasks = [t for t in tasks if t.name in wanted]
    if args.limit and len(tasks) > args.limit:
        # An even spread rather than the first N alphabetically.
        step = len(tasks) / args.limit
        tasks = [tasks[int(i * step)] for i in range(args.limit)]

    env = dict(os.environ)
    mock = None
    if args.dry_run:
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        mock = subprocess.Popen([sys.executable, "-I", os.path.join(ROOT, "bench/mock.py"), str(port)],
                                env=dict(os.environ, MOCK_ROUNDS="1"))
        time.sleep(0.5)
        env.update(ANTHROPIC_API_KEY="mock", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}")

    results, spent = [], 0.0
    print(f"{args.suite}: {len(tasks)} tasks with {args.agent}, budget ${args.budget:.2f}", file=sys.stderr)
    try:
        for i, task in enumerate(tasks, 1):
            known = [r["cost"] for r in results if r["cost"] is not None]
            average = sum(known) / len(known) if known else 0
            if spent + average > args.budget:
                print(f"stopping: ${spent:.2f} spent, the next task would likely pass the budget", file=sys.stderr)
                break
            results.append(run_task(task, args, env))
            r = results[-1]
            spent += r["cost"] or 0
            mark = "pass" if r["passed"] else "FAIL"
            cost = f"${r['cost']:.3f}" if r["cost"] is not None else "$?"
            print(f"[{i}/{len(tasks)}] {mark}  {task.name:<28} {r['seconds']:6.1f}s  {cost}", file=sys.stderr)
    finally:
        if mock:
            mock.kill()

    passed = sum(r["passed"] for r in results)
    print(f"\n{args.suite} with {args.agent}{' (' + args.model + ')' if args.model else ''}: "
          f"{passed}/{len(results)} passed, ${spent:.2f} spent")
    out_dir = os.path.join(ROOT, "bench/results")
    os.makedirs(out_dir, exist_ok=True)
    out = os.path.join(out_dir, time.strftime(f"evals-{args.suite}-{args.agent}-%Y%m%d-%H%M%S.json"))
    with open(out, "w") as f:
        json.dump({"suite": args.suite, "agent": args.agent, "model": args.model, "passed": passed,
                   "total": len(results), "spent_usd": spent, "results": results}, f, indent=2)
    print(f"details: {os.path.relpath(out, ROOT)}", file=sys.stderr)


def run_task(task, args, env):
    work = tempfile.mkdtemp(prefix=f"eval-{task.name}-")
    task.setup(work)
    subprocess.run(["git", "init", "-q"], cwd=work)
    subprocess.run(["git", "add", "-A"], cwd=work)
    subprocess.run(["git", "-c", "user.name=eval", "-c", "user.email=eval@localhost", "commit", "-qm", "start"],
                   cwd=work)
    report = os.path.join(work, ".report.json")
    argv, extra = agent_command(args.agent, task.prompt, args.model, work, report)
    run_env = dict(env, **extra)
    # A private home per run, so sessions and memory don't leak between tasks.
    run_env.update(XDG_DATA_HOME=os.path.join(work, ".data"))

    start = time.time()
    try:
        p = subprocess.run(argv, cwd=work, env=run_env, capture_output=True, text=True, timeout=task.timeout,
                           stdin=subprocess.DEVNULL)
        stdout, agent_ok = p.stdout, p.returncode == 0
    except subprocess.TimeoutExpired as e:
        stdout, agent_ok = (e.stdout or b"").decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or ""), False
    seconds = time.time() - start
    cost = cost_of(args.agent, report, stdout)
    if os.path.exists(report):
        os.remove(report)
    passed, check_output = task.check(work)
    shutil.rmtree(work, ignore_errors=True)
    return {"task": task.name, "passed": passed, "agent_exited_ok": agent_ok, "seconds": round(seconds, 1),
            "cost": cost, "check_output": check_output}


if __name__ == "__main__":
    main()
