#!/usr/bin/env python3
# Hostile tool calls against the real binary, with the fake API playing the
# model: missing and mistyped arguments, huge and odd strings, paths that
# point at devices, pipes, loops and other people's files, tools that don't
# exist. Whatever is asked, claude-jr must stay up (a panic aborts it), answer
# every call, never send the API a conversation it would refuse, and finish.
# Seeds are printed, so a failure can be run again.
#
#   python3 bench/toolfuzz.py target/release/claudejr          # 6 seeds
#   python3 bench/toolfuzz.py target/release/claudejr 20 40    # 20 seeds, 40 rounds each
#   SEED=7 python3 bench/toolfuzz.py target/release/claudejr   # one seed
#   SHOW=1 SEED=7 python3 bench/toolfuzz.py ...                # and print each call with its answer
#
# It runs with every permission, so it runs inside bubblewrap (bwrap): the
# system read-only, a home of its own, and the project on a 256 MB disk of its
# own with a limit on memory. Nothing of yours can be touched or filled up.

import json, os, random, shutil, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ODD = ["", " ", "\n", "\t", "é", "日本語", "👩‍👩‍👧‍👦", "é" * 40, "‮evil", "\x00", "a\x00b", "\x1b[2J", "\r\n",
       "%s%n", "$(id)", "`id`", "; rm -rf .", "' OR 1=1", "x" * 5000, "ü" * 20000, "﻿bom", "\\", "\"", "{}"]
PATHS = ["a.txt", "crlf.txt", "empty.txt", "blob.bin", "big.txt", "dir", "dir/", "dir/deep/new.txt", "loop", "pipe",
         "gone.txt", "..", "../..", "/", "/etc/passwd", "/etc/shadow", "/dev/zero", "/dev/null", "/dev/stdin",
         "/proc/self/mem", "/proc/self/environ", "~", "~/x", "./a.txt/", "a.txt/b", "é 日本.txt", "name\nwith newline",
         "/mnt/proj/" + "d/" * 400 + "f", "x" * 300, "", ".", "link-out", "noperm.txt", "shot.png"]
JUNK = [None, True, False, 0, -1, 1, 2**31, 2**63, 2**64, -2**63, 1.5, 1e308, [], [1], {}, {"a": 1}, "3", "-1"]
COMMANDS = ["true", "false", "exit 7", "echo hi", "printf 'a\\0b\\377\\n'", "cd dir && pwd", "cd /nowhere", "cd ..; cd /; pwd",
            "sleep 0.2", "yes | head -c 300000", "head -c 200000 /dev/urandom", "cat", "read x; echo $x", "echo $$ $PPID",
            "sleep 30 & echo started", "printf '\\033[31mred\\033[0m\\n10%%\\r99%%\\n'", "kill -9 $$", "exec 1>&-; echo x",
            "rm -rf dir/tmp; mkdir -p dir/tmp; cd dir/tmp; rmdir ../tmp; pwd", "seq 1 100000", "echo é日本👍", "ulimit -n",
            "trap '' TERM; sleep 3", "printf '%s' \"$(head -c 70000 /dev/zero | tr '\\0' é)\"", "", "   ", "(", "\x00"]


def value(rng, kind):
    roll = rng.random()
    if roll < 0.12:
        return rng.choice(JUNK)
    if roll < 0.3:
        return rng.choice(ODD)
    return rng.choice({"path": PATHS, "command": COMMANDS, "text": ODD + ["one", "two", "one\ntwo", "TWO"]}[kind])


def call(rng):
    tool = rng.choice(["read", "read", "write", "edit", "edit", "bash", "bash", "bash", "jobs", "todo", "memory", "skill",
                       "agent", "ask_user", "nope", "", "mcp__none__tool", "web_search", "Read"])
    fields = {
        "read": {"path": "path", "offset": None, "limit": None},
        "write": {"path": "path", "content": "text"},
        "edit": {"path": "path", "old_string": "text", "new_string": "text", "replace_all": None},
        "bash": {"command": "command", "timeout": None, "background": None},
        "jobs": {"action": None, "id": None, "until": "text", "timeout": None},
        "todo": {"todos": None},
        "memory": {"action": None, "note": "text"},
        "skill": {"name": "text", "args": "text"},
        "agent": {"description": "text", "prompt": "text"},
    }.get(tool, {"anything": "text"})
    inp = {}
    for key, kind in fields.items():
        if rng.random() < 0.12:
            continue  # left out
        if kind:
            inp[key] = value(rng, kind)
        elif key in ("offset", "limit", "timeout", "id"):
            inp[key] = rng.choice(JUNK + [1, 2, 3, 5, 2000])
        elif key in ("replace_all", "background"):
            inp[key] = rng.choice([True, False, False, "yes", None, 1])
        elif key == "action":
            inp[key] = rng.choice(["list", "wait", "kill", "read", "add", "show", "clear", "", None, 7, "explode"])
        elif key == "todos":
            inp[key] = rng.choice([[], [{"content": "x", "status": "pending"}], [{"content": 1}], "todo", None,
                                   [{"content": "ü" * 3000, "status": "weird"}] * 50])
    if rng.random() < 0.05:
        inp = rng.choice([[], "string", 5, None])  # not an object at all
    return {"name": tool, "input": inp}


# Makes the project the calls are aimed at. It runs inside the box, where the
# project is on a small disk of its own.
SETUP = r"""
import os
os.makedirs("dir")
files = {"a.txt": "one\ntwo\nthree\n", "crlf.txt": "one\r\ntwo\r\n", "empty.txt": "", "big.txt": "line é\n" * 60000,
         "noperm.txt": "secret\n", "é 日本.txt": "ünï\n"}
for name, text in files.items():
    open(name, "w", newline="").write(text)
open("blob.bin", "wb").write(bytes(range(256)) * 40)
open("shot.png", "wb").write(b"\x89PNG\r\n\x1a\nnot really")
os.chmod("noperm.txt", 0)
os.symlink("loop", "loop")
os.symlink("/etc", "link-out")
os.mkfifo("pipe")
"""
# Inside the box: a 256 MB disk at /mnt and 3 GB of address space, so a call
# that tries to fill the disk or the memory fails there and nowhere else.
START = ("ulimit -v 3000000; mkdir -p /mnt/proj /mnt/data /mnt/config && cd /mnt/proj && python3 -I /tmp/seed/setup.py && "
         "exec /tmp/seed/claudejr -y -p go --output-format json")


def attempt(seed, rounds, binary):
    rng = random.Random(seed)
    work = tempfile.mkdtemp(prefix="cjr-fuzz-")
    script = [[call(rng) for _ in range(rng.randint(1, 5))] for _ in range(rounds)]
    json.dump(script, open(f"{work}/script.json", "w"))
    open(f"{work}/setup.py", "w").write(SETUP)
    mock = subprocess.Popen([sys.executable, "-I", f"{ROOT}/bench/mock.py", "0"], stdout=subprocess.PIPE, text=True,
                            env={**os.environ, "MOCK_SCENARIO": "script", "MOCK_SCRIPT": f"{work}/script.json",
                                 "MOCK_LOG": f"{work}/requests"})
    port = mock.stdout.readline().strip()
    box = ["bwrap", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--tmpfs", "/home",
           "--size", str(256 * 1024 * 1024), "--tmpfs", "/mnt", "--ro-bind", work, "/tmp/seed", "--ro-bind", binary,
           "/tmp/seed/claudejr", "--die-with-parent", "--setenv", "HOME", "/home/fuzz", "--setenv", "XDG_DATA_HOME",
           "/mnt/data", "--setenv", "XDG_CONFIG_HOME", "/mnt/config", "--setenv", "ANTHROPIC_API_KEY", "mock",
           "--setenv", "ANTHROPIC_BASE_URL", f"http://127.0.0.1:{port}", "--setenv", "CLAUDE_JR_OFFLINE_SECS", "10"]
    open(f"{work}/claudejr", "w").close()  # where the binary is mounted
    start, problem = time.time(), None
    try:
        run = subprocess.run(box + ["sh", "-c", START], stdin=subprocess.DEVNULL, capture_output=True, timeout=600)
        requests = [json.loads(line) for line in open(f"{work}/requests")] if os.path.exists(f"{work}/requests") else []
        # The main agent's requests; subagents make their own.
        requests = [r for r in requests if "You are a subagent" not in json.dumps(r.get("system", ""))]
        refused = open(f"{work}/requests.invalid").read() if os.path.exists(f"{work}/requests.invalid") else ""
        if run.returncode not in (0, 1):
            problem = f"it died with status {run.returncode}: {run.stderr.decode(errors='replace')[-600:]}"
        elif refused.strip():
            problem = f"it sent what the API would refuse: {refused[:600]}"
        elif len(requests) != rounds + 1 and run.returncode == 0:
            problem = f"{len(requests)} requests for {rounds} rounds, yet it reported success"
        elif run.returncode == 1 and b'"error"' not in run.stdout:
            problem = f"it failed without saying why: {run.stdout[-300:]!r} {run.stderr[-300:]!r}"
        else:
            # Whatever was asked, each call got an answer.
            last = requests[-1]["messages"] if requests else []
            asked = {c["id"] for m in last if m["role"] == "assistant" for c in m["content"] if c.get("type") == "tool_use"}
            told = {c["tool_use_id"] for m in last if m["role"] == "user" and isinstance(m["content"], list)
                    for c in m["content"] if c.get("type") == "tool_result"}
            if asked - told:
                problem = f"calls left unanswered: {sorted(asked - told)[:5]}"
            if os.environ.get("SHOW"):  # each call and the start of its answer
                answers = {c["tool_use_id"]: c for m in last if m["role"] == "user" and isinstance(m["content"], list)
                           for c in m["content"] if c.get("type") == "tool_result"}
                for m in last:
                    for c in (m["content"] if m["role"] == "assistant" else []):
                        if c.get("type") == "tool_use":
                            a = answers[c["id"]]
                            print(f"  {c['name'] or '(no name)':8} {json.dumps(c['input'], ensure_ascii=False)[:70]:70} -> "
                                  f"{'ERR ' if a.get('is_error') else 'ok  '}{json.dumps(a.get('content'), ensure_ascii=False)[:80]}")
    except subprocess.TimeoutExpired:
        problem = "it was still going after 10 minutes"
    finally:
        mock.kill()
    took = time.time() - start
    if problem:
        # The script is enough to run it again; the request log can be large.
        keep = f"{tempfile.gettempdir()}/cjr-fuzz-seed-{seed}.json"
        shutil.copy(f"{work}/script.json", keep)
        print(f"seed {seed}: FAILED after {took:.0f}s: {problem}\n  its calls are in {keep}", flush=True)
    else:
        print(f"seed {seed}: ok ({rounds} rounds, {sum(len(r) for r in script)} calls, {took:.1f}s)", flush=True)
    shutil.rmtree(work, ignore_errors=True)
    return problem is None


if __name__ == "__main__":
    binary = os.path.abspath(sys.argv[1])
    seeds = [int(os.environ["SEED"])] if os.environ.get("SEED") else range(1, int(sys.argv[2]) + 1 if len(sys.argv) > 2 else 7)
    rounds = int(sys.argv[3]) if len(sys.argv) > 3 else 25
    sys.exit(0 if all([attempt(seed, rounds, binary) for seed in seeds]) else 1)
