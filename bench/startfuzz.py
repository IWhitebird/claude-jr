#!/usr/bin/env python3
# Odd and damaged files where claude-jr reads its settings at start-up: the
# config files, the list of MCP servers, the trusted folders, the saved key,
# skills and instruction files, each cut off, mistyped, empty or random bytes.
# Whatever is there, it must start (or say what is wrong with which file),
# never crash, and never send the API a conversation it would refuse.
#
#   python3 bench/startfuzz.py target/release/claudejr         # 40 seeds
#   python3 bench/startfuzz.py target/release/claudejr 150

import json, os, random, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
binary = os.path.abspath(sys.argv[1])
seeds = int(sys.argv[2]) if len(sys.argv) > 2 else 40
JUNK = [None, True, 0, -1, 1.5, 2**64, "", "x", "ü" * 3000, [], [1, "a", None], {}, {"a": {"b": [1]}}, "claude-opus-5-5", "ollama/x", "/", "../.."]
KEYS = ["model", "effort", "mode", "allow", "deny", "worker_model", "providers", "sandbox", "layout", "web", "mcpServers",
        "max_tokens", "theme", "auto", "context", "base_url", "key_env", "api", "enabled", "read", "write", "nonsense"]
def junk(rng, depth=0):
    r = rng.random()
    if depth < 3 and r < 0.25: return {rng.choice(KEYS): junk(rng, depth + 1) for _ in range(rng.randint(0, 5))}
    if depth < 3 and r < 0.35: return [junk(rng, depth + 1) for _ in range(rng.randint(0, 4))]
    return rng.choice(JUNK)
def text(rng, obj):
    t = json.dumps(obj)
    r = rng.random()
    if r < 0.15: t = t[:rng.randrange(len(t) + 1)]
    elif r < 0.25: t = t.replace('"', "'", 1)
    elif r < 0.3: t = "﻿" + t
    elif r < 0.35: return os.urandom(rng.randint(0, 200))
    return t.encode()
bad = 0
for seed in range(1, seeds + 1):
    rng = random.Random(seed)
    work = tempfile.mkdtemp(prefix="cjr-start-")
    proj, cfg = f"{work}/proj", f"{work}/config/claude-jr"
    os.makedirs(proj); os.makedirs(cfg); os.makedirs(f"{proj}/.claude/skills/odd"); os.makedirs(f"{cfg}/skills/two")
    files = {f"{cfg}/config.json": {rng.choice(KEYS): junk(rng) for _ in range(rng.randint(0, 8))},
             f"{proj}/.claude-jr.json": {rng.choice(KEYS): junk(rng) for _ in range(rng.randint(0, 8))},
             f"{proj}/.mcp.json": {"mcpServers": junk(rng)} if rng.random() < 0.7 else junk(rng)}
    for path, obj in files.items():
        if rng.random() < 0.8: open(path, "wb").write(text(rng, obj))
    open(f"{cfg}/trusted", "wb").write(rng.choice([f"{proj}\n".encode(), b"", os.urandom(50), f"{proj}".encode(), b"\n\n/\n"]))
    open(f"{cfg}/credentials", "wb").write(rng.choice([b"mock", b"", b"\n", os.urandom(40), b"sk-ant-x\n\n", b"k" * 100000]))
    for path in (f"{proj}/AGENTS.md", f"{proj}/CLAUDE.md", f"{proj}/.claude/skills/odd/SKILL.md", f"{cfg}/skills/two/SKILL.md",
                 f"{cfg}/AGENTS.md"):
        if rng.random() < 0.7:
            open(path, "wb").write(rng.choice([b"", os.urandom(300), b"---\n", b"---\nname: x\n", b"---\nname: \ndescription: \n---\n",
                                               b"---\nname: " + b"n" * 5000 + b"\ndescription: d\n---\nbody", "# 見出し\n".encode() * 20000,
                                               b"---\nname: ok\ndescription: fine\n---\nDo it.\n", b"\xff\xfe\x00\x00", b"- note\n" * 50]))
    mock = subprocess.Popen([sys.executable, "-I", f"{ROOT}/bench/mock.py", "0"], stdout=subprocess.PIPE, text=True,
                            env={**os.environ, "MOCK_LOG": f"{work}/requests", "MOCK_ROUNDS": "1"})
    port = mock.stdout.readline().strip()
    env = {"PATH": os.environ["PATH"], "HOME": work, "XDG_CONFIG_HOME": f"{work}/config", "XDG_DATA_HOME": f"{work}/data",
           "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "CLAUDE_JR_OFFLINE_SECS": "3"}
    if rng.random() < 0.5: env["ANTHROPIC_API_KEY"] = "mock"
    outs = []
    for args in (["-p", "hi", "-y"], ["--sessions"], ["task", "list"], ["--help"], ["-p", "again", "-c"]):
        try:
            r = subprocess.run([binary] + args, cwd=proj, env=env, capture_output=True, timeout=60, stdin=subprocess.DEVNULL)
            outs.append(r.returncode)
            if r.returncode not in (0, 1, 2): print("   ", args, r.stderr.decode(errors="replace")[-300:])
        except subprocess.TimeoutExpired:
            outs.append("TIMEOUT")
    mock.kill()
    refused = open(f"{work}/requests.invalid").read().strip() if os.path.exists(f"{work}/requests.invalid") else ""
    ok = all(o in (0, 1, 2) for o in outs) and not refused
    if not ok:
        bad += 1
        print(f"seed {seed}: FAIL exits {outs} {refused[:200]}")
    shutil.rmtree(work, ignore_errors=True)
print(bad, "failed of", seeds)
sys.exit(1 if bad else 0)
