#!/usr/bin/env python3
# Compares claude-jr with other coding agents on resources, using the same
# fake API (bench/mock.py) for all of them, so no API key is needed and the
# model's speed doesn't count.
#
#   python3 bench/compare.py                       # every agent it can find
#   python3 bench/compare.py claude-jr pi          # some of them
#   ROUNDS=40 IDLE=10 python3 bench/compare.py
#
# Agents and where they're looked for (override with the env variable):
#   claude-jr    CLAUDE_JR_BIN  target/x86_64-unknown-linux-musl/release/claude-jr, then target/release/
#   claude-code  CLAUDE_BIN     `claude` on PATH
#   pi           PI_BIN         `pi` on PATH
#   codex        CODEX_BIN      `codex` on PATH (idle only: it speaks OpenAI's API, not Anthropic's)
#
# For each agent:
#   size   bytes on disk (for npm packages, the node_modules they were
#          installed into, so install each one into its own prefix; Node
#          itself isn't counted)
#   idle   the interactive UI in a pseudo-terminal: time until the screen
#          settles, memory, threads, and CPU used while left alone
#   busy   one prompt in print mode, ROUNDS bash calls with streamed text in
#          between: wall time, the agent's own CPU and peak memory (sampled
#          from /proc), and the total including the commands it ran
#
# Each agent runs with a fresh, empty HOME so your own config isn't touched.

import json, os, pty, resource, select, shutil, signal, socket, struct, subprocess, sys, tempfile, time
import fcntl, termios

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROUNDS = int(os.environ.get("ROUNDS", "20"))
IDLE = float(os.environ.get("IDLE", "5"))
MODEL = "claude-opus-5-5"
PROMPT = "Run the build a few times and tell me what you see."
KEY = "sk-ant-api03-mock-key-for-benchmarks-000000000000"
TICK = os.sysconf("SC_CLK_TCK")


def find(env, *candidates):
    if os.environ.get(env):
        return os.path.realpath(os.environ[env])
    for c in candidates:
        path = c if os.path.isabs(c) else shutil.which(c) or os.path.join(ROOT, c)
        if path and os.path.isfile(path):
            return os.path.realpath(path)
    return None


def install_size(binary):
    """The binary itself, or for an npm package everything in the node_modules
    it was installed into (its dependencies included; Node itself isn't)."""
    parts = binary.split("/node_modules/")
    if len(parts) == 1:
        return os.path.getsize(binary)
    out = subprocess.run(["du", "-sb", parts[0] + "/node_modules"], capture_output=True, text=True).stdout.split()
    return int(out[0]) if out else 0


# ---- per-agent setup --------------------------------------------------------

def setup(name, home, port):
    """Writes config into the fake HOME. Returns env and the argv builders."""
    base = f"http://127.0.0.1:{port}"
    env = dict(os.environ, HOME=home, ANTHROPIC_API_KEY=KEY, ANTHROPIC_BASE_URL=base, TERM="xterm-256color",
               XDG_CONFIG_HOME=f"{home}/.config", XDG_DATA_HOME=f"{home}/.local/share",
               XDG_CACHE_HOME=f"{home}/.cache")
    if name == "claude-jr":
        return env, lambda b: [b], lambda b: [b, "-y", "-p", PROMPT, "-m", MODEL]
    if name == "claude-code":
        env.update(DISABLE_AUTOUPDATER="1", DISABLE_TELEMETRY="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
        with open(f"{home}/.claude.json", "w") as f:
            json.dump({"hasCompletedOnboarding": True, "bypassPermissionsModeAccepted": True,
                       "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []}}, f)
        return env, lambda b: [b], lambda b: [b, "-p", PROMPT, "--model", MODEL, "--dangerously-skip-permissions"]
    if name == "pi":
        agent_dir = f"{home}/.pi/agent"
        os.makedirs(agent_dir)
        with open(f"{agent_dir}/models.json", "w") as f:
            json.dump({"providers": {"anthropic": {"baseUrl": base, "apiKey": KEY}}}, f)
        env.update(PI_CODING_AGENT_DIR=agent_dir)
        model = ["--provider", "anthropic", "--model", MODEL]
        return env, lambda b: [b, *model], lambda b: [b, "--print", PROMPT, *model]
    if name == "codex":
        return env, lambda b: [b], None
    raise ValueError(name)


AGENTS = {
    "claude-jr": lambda: find("CLAUDE_JR_BIN", "target/x86_64-unknown-linux-musl/release/claude-jr",
                              "target/release/claude-jr"),
    "claude-code": lambda: find("CLAUDE_BIN", "claude"),
    "pi": lambda: find("PI_BIN", "pi"),
    "codex": lambda: find("CODEX_BIN", "codex"),
}


# ---- measuring ----------------------------------------------------------------

def proc(pid):
    """(cpu seconds, rss KB, peak rss KB, threads) for one process, or None."""
    try:
        stat = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        status = {l.split(":")[0]: l.split()[1] for l in open(f"/proc/{pid}/status") if ":" in l and len(l.split()) > 1}
    except (FileNotFoundError, ProcessLookupError, IndexError):
        return None
    cpu = (int(stat[11]) + int(stat[12])) / TICK
    return cpu, int(status.get("VmRSS", 0)), int(status.get("VmHWM", 0)), int(status.get("Threads", 0))


def tree(pid):
    """pid and all its descendants."""
    out, todo = [], [pid]
    while todo:
        p = todo.pop()
        out.append(p)
        try:
            for t in os.listdir(f"/proc/{p}/task"):
                with open(f"/proc/{p}/task/{t}/children") as f:
                    todo += [int(c) for c in f.read().split()]
        except (FileNotFoundError, ProcessLookupError):
            pass
    return out


def idle(argv, env, cwd):
    """Starts the UI in a pty and leaves it alone."""
    start = time.perf_counter()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execvpe(argv[0], argv, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))

    def drain(seconds):
        """Reads output for `seconds`, answering terminal queries. Returns
        the time of the last output."""
        last, end = None, time.time() + seconds
        while time.time() < end:
            if select.select([fd], [], [], 0.02)[0]:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    return last
                if b"\x1b[c" in chunk or b"\x1b[0c" in chunk:
                    os.write(fd, b"\x1b[?62c")
                if b"\x1b[6n" in chunk:
                    os.write(fd, b"\x1b[1;1R")
                if b"\x1b]11;?" in chunk:
                    os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
                last = time.perf_counter()
        return last

    # Settled: no output for 500 ms, within 15 s.
    settled, deadline = None, time.time() + 15
    while time.time() < deadline:
        last = drain(0.5)
        if last is None and settled is not None:
            break
        settled = last or settled
    settle_ms = (settled - start) * 1000 if settled else None

    procs = tree(pid)
    before = sum((proc(p) or (0,))[0] for p in procs)
    drain(IDLE)
    procs = tree(pid)
    stats = [s for s in (proc(p) for p in procs) if s]
    after = sum(s[0] for s in stats)
    result = {
        "settle_ms": settle_ms,
        "rss_kb": sum(s[1] for s in stats),
        "threads": sum(s[3] for s in stats),
        "processes": len(stats),
        "idle_cpu_ms": (after - before) * 1000,
    }
    for p in reversed(procs):
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    os.waitpid(pid, 0)
    os.close(fd)
    return result


def busy(argv, env, cwd):
    """One prompt in print mode; samples the agent's own process while it runs."""
    start = time.perf_counter()
    child = subprocess.Popen(argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    fcntl.fcntl(child.stdout, fcntl.F_SETFL, os.O_NONBLOCK)
    own_cpu, peak = 0.0, 0
    output = b""
    while True:
        s = proc(child.pid)
        if s:
            own_cpu, peak = max(own_cpu, s[0]), max(peak, s[2])
        try:
            output += child.stdout.read() or b""
        except (BlockingIOError, TypeError):
            pass
        if child.poll() is not None:
            break
        time.sleep(0.01)
    wall = time.perf_counter() - start
    return {
        "ok": child.returncode == 0,
        "wall_s": wall,
        "own_cpu_s": own_cpu,
        "own_peak_kb": peak,
        "tail": output.decode(errors="replace").strip().splitlines()[-1:] if output else [],
    }


def run_agent(name, binary, port):
    home = tempfile.mkdtemp(prefix=f"cmp-{name}-")
    project = os.path.join(home, "project")
    os.makedirs(project)
    subprocess.run(["git", "init", "-q"], cwd=project)
    env, interactive, oneshot = setup(name, home, port)
    result = {"agent": name, "binary": binary, "size": install_size(binary)}
    result["idle"] = idle(interactive(binary), env, project)
    if oneshot:
        # Totals for everything the run started, from the kernel's accounting.
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        result["busy"] = busy(oneshot(binary), env, project)
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        result["busy"]["total_cpu_s"] = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    shutil.rmtree(home, ignore_errors=True)
    return result


# ---- report -------------------------------------------------------------------

def mb(kb):
    return f"{kb / 1024:.1f} MB" if kb else "-"


def table(results):
    rows = [("", *[r["agent"] for r in results])]
    rows.append(("install size", *[f"{r['size'] / 1048576:.1f} MB" for r in results]))
    rows.append(("startup (screen settles)", *[f"{r['idle']['settle_ms']:.0f} ms" if r["idle"]["settle_ms"] else "-" for r in results]))
    rows.append(("memory, idle", *[mb(r["idle"]["rss_kb"]) for r in results]))
    rows.append(("processes / threads, idle", *[f"{r['idle']['processes']} / {r['idle']['threads']}" for r in results]))
    rows.append((f"CPU while idle {IDLE:.0f}s", *[f"{r['idle']['idle_cpu_ms']:.0f} ms" for r in results]))
    def b(r, key, fmt):
        return fmt(r["busy"][key]) if r.get("busy") and r["busy"]["ok"] else ("failed" if r.get("busy") else "n/a")
    rows.append((f"busy: {ROUNDS} bash calls, wall", *[b(r, "wall_s", lambda v: f"{v:.2f} s") for r in results]))
    rows.append(("busy: agent's own CPU", *[b(r, "own_cpu_s", lambda v: f"{v * 1000:.0f} ms") for r in results]))
    rows.append(("busy: agent's own peak memory", *[b(r, "own_peak_kb", mb) for r in results]))
    rows.append(("busy: CPU incl. commands", *[b(r, "total_cpu_s", lambda v: f"{v * 1000:.0f} ms") for r in results]))
    widths = [max(len(str(row[i])) for row in rows) for i in range(len(rows[0]))]
    lines = []
    for i, row in enumerate(rows):
        lines.append("| " + " | ".join(str(c).ljust(w) for c, w in zip(row, widths)) + " |")
        if i == 0:
            lines.append("|" + "|".join("-" * (w + 2) for w in widths) + "|")
    return "\n".join(lines)


def main():
    names = sys.argv[1:] or list(AGENTS)
    found = [(n, AGENTS[n]()) for n in names]
    for n, b in found:
        if not b:
            print(f"skipping {n}: not found", file=sys.stderr)
    found = [(n, b) for n, b in found if b]

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    mock_env = dict(os.environ, MOCK_SCENARIO="bench", MOCK_ROUNDS=str(ROUNDS), MOCK_OUTPUT_KB="4")
    mock = subprocess.Popen([sys.executable, "-I", os.path.join(ROOT, "bench/mock.py"), str(port)], env=mock_env)
    time.sleep(0.5)
    try:
        results = []
        for n, b in found:
            print(f"measuring {n} ...", file=sys.stderr)
            results.append(run_agent(n, b, port))
    finally:
        mock.kill()

    print(table(results))
    os.makedirs(os.path.join(ROOT, "bench/results"), exist_ok=True)
    out = os.path.join(ROOT, "bench/results", time.strftime("compare-%Y%m%d-%H%M%S.json"))
    with open(out, "w") as f:
        json.dump({"rounds": ROUNDS, "idle_seconds": IDLE, "results": results}, f, indent=2)
    print(f"\nraw numbers: {os.path.relpath(out, ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
