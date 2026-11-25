#!/usr/bin/env python3
# An MCP server that misbehaves, a different way for each seed: answers that
# are no objects, tool lists full of junk, results of the wrong shape, lines
# that are not JSON or not UTF-8, requests of its own, replies in a list,
# dying half-way. claude-jr, with the fake API calling its tools, must not
# crash, hang or send the API a conversation it would refuse.
#
#   python3 bench/mcpfuzz.py target/release/claudejr         # 40 seeds
#   python3 bench/mcpfuzz.py target/release/claudejr 200

import json, os, random, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The server itself, written to a file and started by claude-jr.
SERVER = r'''
import json, os, random, sys, time
rng = random.Random(os.environ.get("EVIL_SEED", "1"))
out = sys.stdout.buffer
def say(obj):
    out.write((obj if isinstance(obj, bytes) else json.dumps(obj).encode()) + b"\n"); out.flush()
JUNK = [None, True, 5, -1, 1.5, "", "text", [], [1, None, "x"], {}, {"a": 1}, "ü" * 2000]
def tools():
    kind = rng.choice(["ok", "ok", "ok", "not-array", "junk-entries", "dups", "many", "builtin-names", "odd-names", "cursor", "odd-schema"])
    ok = [{"name": n, "description": f"the {n} tool", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}
          for n in ("echo", "big", "odd")]
    if kind == "not-array": return {"tools": rng.choice(JUNK)}
    if kind == "junk-entries": return {"tools": ok + [rng.choice(JUNK) for _ in range(4)] + [{"name": rng.choice(JUNK)}, {"description": "no name"}]}
    if kind == "dups": return {"tools": ok + ok + ok}
    if kind == "many": return {"tools": ok + [{"name": f"t{i}", "description": "x" * 300, "inputSchema": {"type": "object"}} for i in range(400)]}
    if kind == "builtin-names": return {"tools": ok + [{"name": n, "inputSchema": {"type": "object"}} for n in ("bash", "read", "write", "edit", "agent")]}
    if kind == "odd-names": return {"tools": ok + [{"name": n, "inputSchema": {"type": "object"}} for n in ("with space", "ünï", "a" * 300, "", "x__y", "../up", "\n", "\x1b[2J")]}
    if kind == "cursor": return {"tools": ok, "nextCursor": "again"}
    if kind == "odd-schema": return {"tools": [dict(t, inputSchema=rng.choice(JUNK + [{"type": "object", "properties": {"deep": {"type": "array", "items": {"$ref": "#"}}}}]), description=rng.choice(JUNK)) for t in ok]}
    return {"tools": ok}
def result():
    kind = rng.choice(["ok", "ok", "ok", "not-array", "junk-items", "is-error", "odd-error", "null", "big", "image", "resource", "structured"])
    if kind == "not-array": return {"content": rng.choice(JUNK)}
    if kind == "junk-items": return {"content": [rng.choice(JUNK) for _ in range(3)] + [{"type": "text", "text": rng.choice(JUNK)}, {"type": rng.choice(JUNK)}]}
    if kind == "is-error": return {"content": [{"type": "text", "text": "it failed"}], "isError": rng.choice([True, "yes", 1, None])}
    if kind == "null": return None
    if kind == "big": return {"content": [{"type": "text", "text": "é" * 2_000_000}]}
    if kind == "image": return {"content": [{"type": "image", "data": rng.choice(["aGk=", "not base64!", 5, ""]), "mimeType": rng.choice(["image/png", "text/x", None])}]}
    if kind == "resource": return {"content": [{"type": "resource", "resource": rng.choice(JUNK + [{"uri": "file:///x", "text": "body"}])}, {"type": "resource_link", "uri": 5}]}
    if kind == "structured": return {"structuredContent": {"a": [1, 2]}, "content": []}
    return {"content": [{"type": "text", "text": "pong"}]}
first = rng.choice(["ok", "ok", "ok", "garbage-first", "not-object", "error", "no-result", "exit", "slow", "wrong-version"])
for line in sys.stdin.buffer:
    try: msg = json.loads(line)
    except Exception: continue
    if not isinstance(msg, dict) or "id" not in msg or "method" not in msg: continue
    mid, method = msg["id"], msg["method"]
    if rng.random() < 0.15: say({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": rng.choice(JUNK)}})
    if rng.random() < 0.08: say({"jsonrpc": "2.0", "id": f"srv{rng.random()}", "method": rng.choice(["sampling/createMessage", "roots/list", "ping", "elicitation/create", "nope"]), "params": {}})
    if rng.random() < 0.05: say(rng.choice([b"\xff\xfe not json", b"", b"[]", b"{}", b'{"jsonrpc":"2.0","id":"nobody","result":{}}', b"null", b"x" * 100000]))
    if method == "initialize":
        if first == "exit": sys.exit(0)
        if first == "slow": time.sleep(1.5)
        if first == "garbage-first": say(b"Starting server v1.2 ... ok")
        if first == "not-object": say({"jsonrpc": "2.0", "id": mid, "result": rng.choice(JUNK)}); continue
        if first == "error": say({"jsonrpc": "2.0", "id": mid, "error": rng.choice(JUNK + [{"code": "x"}, {"code": -1, "message": 5}])}); continue
        if first == "no-result": say({"jsonrpc": "2.0", "id": mid}); continue
        say({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": "1999-01-01" if first == "wrong-version" else "2025-06-18",
                                                      "capabilities": rng.choice([{"tools": {}}, {}, None, 5]), "serverInfo": rng.choice([{"name": "evil", "version": "1"}, None, "x"])}})
    elif method == "tools/list":
        say({"jsonrpc": "2.0", "id": mid, "result": tools()})
    elif method == "tools/call":
        how = rng.choice(["ok"] * 6 + ["error", "wrong-id", "die", "twice", "batch", "no-fields"])
        if how == "error": say({"jsonrpc": "2.0", "id": mid, "error": rng.choice(JUNK + [{"code": -32000, "message": "boom"}, {"message": None}])})
        elif how == "wrong-id": say({"jsonrpc": "2.0", "id": "someone else", "result": result()}); say({"jsonrpc": "2.0", "id": mid, "result": result()})
        elif how == "die": sys.exit(3)
        elif how == "twice": r = {"jsonrpc": "2.0", "id": mid, "result": result()}; say(r); say(r)
        elif how == "batch": say(json.dumps([{"jsonrpc": "2.0", "id": mid, "result": result()}]).encode())
        elif how == "no-fields": say({"jsonrpc": "2.0", "id": mid})
        else: say({"jsonrpc": "2.0", "id": mid, "result": result()})
    else:
        say({"jsonrpc": "2.0", "id": mid, "result": {}})
'''

binary = os.path.abspath(sys.argv[1])
seeds = int(sys.argv[2]) if len(sys.argv) > 2 else 40
bad = 0
for seed in range(1, seeds + 1):
    rng = random.Random(seed)
    work = tempfile.mkdtemp(prefix="cjr-mcp-")
    open(f"{work}/server.py", "w").write(SERVER)
    proj, cfg = f"{work}/proj", f"{work}/config/claude-jr"
    os.makedirs(proj); os.makedirs(cfg)
    json.dump({"mcpServers": {"evil": {"command": sys.executable, "args": ["-I", f"{work}/server.py"], "env": {"EVIL_SEED": str(seed)}}}}, open(f"{proj}/.mcp.json", "w"))
    open(f"{cfg}/trusted", "w").write(proj + "\n")
    names = ["mcp__evil__echo", "mcp__evil__big", "mcp__evil__odd", "mcp__evil__t5", "mcp__evil__bash", "mcp__evil__nope", "mcp__evil__with space", "mcp__evil__ünï", "read"]
    script = [[{"name": rng.choice(names), "input": rng.choice([{"text": "hi"}, {}, {"path": ".mcp.json"}, {"text": "é" * 5000}])} for _ in range(rng.randint(1, 4))] for _ in range(8)]
    json.dump(script, open(f"{work}/script.json", "w"))
    mock = subprocess.Popen([sys.executable, "-I", f"{ROOT}/bench/mock.py", "0"], stdout=subprocess.PIPE, text=True,
                            env={**os.environ, "MOCK_LOG": f"{work}/requests", "MOCK_SCENARIO": "script", "MOCK_SCRIPT": f"{work}/script.json"})
    port = mock.stdout.readline().strip()
    env = {"PATH": os.environ["PATH"], "HOME": work, "XDG_CONFIG_HOME": f"{work}/config", "XDG_DATA_HOME": f"{work}/data", "ANTHROPIC_API_KEY": "mock",
           "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "CLAUDE_JR_OFFLINE_SECS": "3"}
    try:
        r = subprocess.run([binary, "-p", "go", "-y", "--output-format", "json"], cwd=proj, env=env, capture_output=True, timeout=150, stdin=subprocess.DEVNULL)
        code, err = r.returncode, r.stderr.decode(errors="replace")
    except subprocess.TimeoutExpired as e:
        code, err = "TIMEOUT", (e.stderr or b"").decode(errors="replace")
    mock.kill()
    refused = open(f"{work}/requests.invalid").read().strip() if os.path.exists(f"{work}/requests.invalid") else ""
    if code not in (0, 1) or refused:
        bad += 1
        at = max(err.rfind("panicked"), err.rfind("memory allocation"), 0)
        print(f"seed {seed}: FAIL exit {code} {refused[:200]} {err[at:at + 400] if at else err[-300:]}", flush=True)
    shutil.rmtree(work, ignore_errors=True)
print(bad, "failed of", seeds, flush=True)
sys.exit(1 if bad else 0)
