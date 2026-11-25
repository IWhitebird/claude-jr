#!/usr/bin/env python3
# Damaged session files: a line cut off, lost from the middle, turned to
# garbage, or changed at random. Whatever happened to the file, listing the
# sessions and continuing that one (twice) must not crash, and must not send
# the API a conversation it would refuse. Uses the fake API.
#
#   python3 bench/damage.py target/release/claudejr          # 23 kinds of damage and 12 random ones
#   RANDOM_CASES=300 python3 bench/damage.py target/release/claudejr

import json, os, random, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
binary = os.path.abspath(sys.argv[1])
work = tempfile.mkdtemp(prefix="cjr-damage-")
proj = f"{work}/proj"; os.makedirs(proj)
env = {**os.environ, "HOME": work, "XDG_DATA_HOME": f"{work}/data", "XDG_CONFIG_HOME": f"{work}/config",
       "ANTHROPIC_API_KEY": "mock", "CLAUDE_JR_OFFLINE_SECS": "10"}
def mock(**extra):
    m = subprocess.Popen([sys.executable, "-I", f"{ROOT}/bench/mock.py", "0"], stdout=subprocess.PIPE, text=True,
                         env={**os.environ, "MOCK_LOG": f"{work}/requests", **extra})
    return m, m.stdout.readline().strip()
m, port = mock()
env["ANTHROPIC_BASE_URL"] = f"http://127.0.0.1:{port}"
subprocess.run([binary, "-y", "-p", "do the demo"], cwd=proj, env=env, capture_output=True)
sdir = os.path.dirname([os.path.join(d, f) for d, _, fs in os.walk(f"{work}/data") for f in fs if f.endswith(".jsonl")][0])
good = open([f"{sdir}/{f}" for f in os.listdir(sdir) if f.endswith(".jsonl")][0], "rb").read()
lines = good.split(b"\n")[:-1]
print("a good session has", len(lines), "lines,", len(good), "bytes")
U = b'{"role":"user","content":[{"type":"text","text":"hi"}]}'
A = b'{"role":"assistant","content":[{"type":"text","text":"hello"}]}'
cases = {
    "cut in the middle of the last line": good[:-40],
    "cut in the middle of a tool call line": b"\n".join(lines[:2]) + b"\n" + lines[2][:len(lines[2]) // 2],
    "no newline at the end": good[:-1],
    "garbage bytes as a line": b"\n".join(lines[:3] + [b"\xff\xfe\x00garbage\x80"] + lines[3:]) + b"\n",
    "a line of valid JSON that is no message": b"\n".join(lines[:3] + [b'[1,2,3]', b'"text"', b'{"role":5}', b'{}', b'null'] + lines[3:]) + b"\n",
    "blank lines": b"\n\n".join(lines) + b"\n\n\n",
    "windows line endings": b"\r\n".join(lines) + b"\r\n",
    "only a header": lines[0] + b"\n",
    "no header": b"\n".join(lines[1:]) + b"\n",
    "header twice": lines[0] + b"\n" + good,
    "empty file": b"",
    "one huge line": lines[0] + b"\n" + b'{"role":"user","content":[{"type":"text","text":"' + b"x" * 3_000_000 + b'"}]}\n' + A + b"\n",
    "ends on a tool call with no result": b"\n".join(lines[:3]) + b"\n",
    "two user messages in a row": lines[0] + b"\n" + U + b"\n" + U + b"\n" + A + b"\n",
    "starts with the assistant": lines[0] + b"\n" + A + b"\n" + U + b"\n" + A + b"\n",
    "two assistant messages in a row": lines[0] + b"\n" + U + b"\n" + A + b"\n" + A + b"\n",
    "truncate past the end": good + b'{"meta":{"truncate":999}}\n',
    "truncate to zero": good + b'{"meta":{"truncate":0}}\n',
    "odd meta": good + b'{"meta":{"truncate":"x","name":5}}\n{"meta":null}\n{"meta":{"name":"' + b"n" * 5000 + b'"}}\n',
    "a message with empty content": lines[0] + b"\n" + b'{"role":"user","content":[]}\n' + A + b"\n",
    "content is a string": lines[0] + b"\n" + b'{"role":"user","content":"plain"}\n' + A + b"\n",
    "a result for a call nobody made": lines[0] + b"\n" + U + b"\n" + A + b"\n" + b'{"role":"user","content":[{"type":"tool_result","tool_use_id":"ghost","content":"x"}]}\n',
    "nul bytes": good.replace(b"demo", b"de\x00mo"),
}
rng = random.Random(1)
for i in range(int(os.environ.get("RANDOM_CASES", "12"))):   # and some random damage
    b = bytearray(good)
    for _ in range(rng.randint(1, 6)):
        at = rng.randrange(len(b))
        op = rng.choice(["flip", "cut", "dup", "del"])
        if op == "flip": b[at] = rng.randrange(256)
        elif op == "cut": b = b[:at]
        elif op == "dup": b[at:at] = b[max(0, at - 50):at]
        else: del b[at:at + rng.randint(1, 80)]
        if not b: break
    cases[f"random damage {i}"] = bytes(b)
m.kill()
bad = 0
for name, data in cases.items():
    shutil.rmtree(sdir); os.makedirs(sdir)
    open(f"{sdir}/1700000000000-1.jsonl", "wb").write(data)
    for f in ("requests", "requests.invalid"):
        if os.path.exists(f"{work}/{f}"): os.remove(f"{work}/{f}")
    m, port = mock(MOCK_ROUNDS="1")
    env["ANTHROPIC_BASE_URL"] = f"http://127.0.0.1:{port}"
    outs = []
    for args in (["--sessions"], ["-y", "-c", "-p", "carry on", "--output-format", "json"], ["-y", "-c", "-p", "and again"]):
        try:
            r = subprocess.run([binary] + args, cwd=proj, env=env, capture_output=True, timeout=60, stdin=subprocess.DEVNULL)
            outs.append(r.returncode)
        except subprocess.TimeoutExpired:
            outs.append("TIMEOUT")
    m.kill()
    refused = open(f"{work}/requests.invalid").read().strip() if os.path.exists(f"{work}/requests.invalid") else ""
    ok = all(o in (0, 1) for o in outs) and not refused
    bad += not ok
    print(f"{'ok  ' if ok else 'FAIL'} {name:45} exits {outs} {('refused: ' + refused[:150]) if refused else ''}")
shutil.rmtree(work, ignore_errors=True)
print(bad, "failed of", len(cases))
sys.exit(1 if bad else 0)
