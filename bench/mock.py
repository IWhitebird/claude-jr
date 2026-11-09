# A fake Messages API for tests and benchmarks. No network, no key.
#
# Each reply either calls tools or, once MOCK_ROUNDS rounds of tool results have
# come back, finishes with a short markdown answer. Every round runs a bash
# command printing MOCK_OUTPUT_KB of output, so the history grows like a real
# long session. The first round also writes and edits a file. MOCK_CMD
# replaces the bash command and MOCK_LOG records every request body.
#
#   python3 bench/mock.py PORT

import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROUNDS = int(os.environ.get("MOCK_ROUNDS", "2"))
OUTPUT_KB = int(os.environ.get("MOCK_OUTPUT_KB", "1"))
LOG = os.environ.get("MOCK_LOG")
CMD = os.environ.get("MOCK_CMD")  # replaces the bash command of every round
SCENARIO = os.environ.get("MOCK_SCENARIO", "default")  # or "features", "bench", "web", "memory"
TEXT_CHARS = int(os.environ.get("MOCK_TEXT_CHARS", "2000"))  # "bench": streamed text per reply
DELAY_MS = float(os.environ.get("MOCK_DELAY_MS", "0"))  # pause between events, like a real stream


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def block(i, start, deltas):
    return ([{"type": "content_block_start", "index": i, "content_block": start}]
            + [{"type": "content_block_delta", "index": i, "delta": d} for d in deltas]
            + [{"type": "content_block_stop", "index": i}])


def reply(blocks, stop, usage):
    ev = [{"type": "message_start", "message": {"usage": usage}}]
    for i, (start, deltas) in enumerate(blocks):
        ev += block(i, start, deltas)
    ev += [{"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 40}},
           {"type": "message_stop"}]
    return ev


def tool(id, name, inp):
    s = json.dumps(inp)
    half = len(s) // 2
    return ({"type": "tool_use", "id": id, "name": name, "input": {}},
            [{"type": "input_json_delta", "partial_json": s[:half]},
             {"type": "input_json_delta", "partial_json": s[half:]}])


def tool_round(n, usage):
    note = f"Round {n + 1}: checking the output."
    blocks = [({"type": "thinking", "thinking": ""},
               [{"type": "thinking_delta", "thinking": note}, {"type": "signature_delta", "signature": "sig"}])]
    cmd = CMD or f"head -c {OUTPUT_KB * 1024} /dev/zero | tr '\\0' x | fold -w 99"
    blocks.append(tool(f"b{n}", "bash", {"command": cmd}))
    if n == 0:
        blocks.append(tool("w0", "write", {"path": "demo.txt", "content": "one\ntwo\nthree\n"}))
        blocks.append(tool("r0", "read", {"path": "demo.txt"}))
        blocks.append(tool("e0", "edit", {"path": "demo.txt", "old_string": "two", "new_string": "TWO"}))
    return reply(blocks, "tool_use", usage)


def final(usage):
    text = ("Done. Here's what happened:\n\n"
            "- wrote `demo.txt`\n- changed **two** to **TWO**\n\n"
            "| file | change | lines |\n|---|---|---:|\n| `demo.txt` | two → **TWO** | 1 |\n| notes.md | none | 0 |\n\n"
            "```sh\ncat demo.txt\n```\n")
    return reply([({"type": "text", "text": ""}, [{"type": "text_delta", "text": text[i:i + 20]}
                                                  for i in range(0, len(text), 20)])], "end_turn", usage)


def text_reply(text, usage):
    return reply([({"type": "text", "text": ""}, [{"type": "text_delta", "text": text}])], "end_turn", usage)


def features(body, done, usage):
    """Exercises every tool family, one step per round of tool results."""
    system = body.get("system", "")
    if "You review actions" in system:          # auto mode's reviewer
        action = body["messages"][-1]["content"][0]["text"]
        return text_reply("BLOCK: deletes files" if "rm -rf" in action else "ALLOW", usage)
    if "You are a subagent" in system:          # a subagent: read a file, then report
        if done == 0:
            return reply([tool("s1", "read", {"path": "notes.txt"})], "tool_use", usage)
        return text_reply("Subagent report: notes.txt has 2 lines.", usage)
    steps = [
        [tool("t1", "todo", {"todos": [
            {"content": "look around", "status": "in_progress"},
            {"content": "report", "status": "pending"}]}),
         tool("q1", "ask_user", {"question": "Which color?", "options": ["red", "blue"]})],
        [tool("a1", "agent", {"description": "check notes", "prompt": "Read notes.txt"}),
         tool("a2", "agent", {"description": "check notes again", "prompt": "Read notes.txt"})],
        [tool("b1", "bash", {"command": "sleep 0.3; echo served; sleep 30", "background": True}),
         tool("j1", "jobs", {"action": "wait", "id": 1, "until": "served", "timeout": 5})],
        [tool("k1", "skill", {"name": "demo"}),
         tool("m1", "mcp__demo__echo", {"text": "ping"})],
        [tool("c1", "bash", {"command": "echo reviewed-and-allowed"}),
         tool("c2", "bash", {"command": "rm -rf ./build"})],
    ]
    if done < len(steps):
        return reply(steps[done], "tool_use", usage)
    return text_reply("All features checked.\n", usage)


def bench(body, done, usage):
    """Works with any agent: finds its bash tool by name, streams text in small
    pieces like the real API, and runs MOCK_ROUNDS commands."""
    names = [t.get("name", "") for t in body.get("tools", [])]
    bash = next((n for n in names if n.lower() == "bash"), None)
    words = ("The build output looks fine so far, checking the next part of it. " * 200)[:TEXT_CHARS]
    text = [{"type": "text_delta", "text": words[i:i + 12]} for i in range(0, len(words), 12)]
    blocks = [({"type": "text", "text": ""}, text)]
    if bash and done < ROUNDS:
        cmd = f"head -c {OUTPUT_KB * 1024} /dev/zero | tr '\\0' x | fold -w 99"
        blocks.append(tool(f"b{done}", bash, {"command": cmd, "description": "print output"}))
        return reply(blocks, "tool_use", usage)
    return reply(blocks, "end_turn", usage)


def web(usage):
    """What a reply looks like after Anthropic ran a web search on its side."""
    results = [{"type": "web_search_result", "url": f"https://example.com/{i}", "title": f"Result {i}",
                "encrypted_content": "x"} for i in range(3)]
    return reply([
        ({"type": "server_tool_use", "id": "srv1", "name": "web_search", "input": {"query": "vite config"}}, []),
        ({"type": "web_search_tool_result", "tool_use_id": "srv1", "content": results}, []),
        ({"type": "text", "text": ""}, [{"type": "text_delta", "text": "Vite reads vite.config.ts.\n"}]),
    ], "end_turn", usage)


def as_message(events):
    """Turns a streamed reply back into one JSON message, for stream: false."""
    content, stop = [], "end_turn"
    for e in events:
        if e["type"] == "content_block_start":
            content.append(dict(e["content_block"]))
        elif e["type"] == "content_block_delta":
            d, b = e["delta"], content[-1]
            if d["type"] == "text_delta":
                b["text"] = b.get("text", "") + d["text"]
            elif d["type"] == "input_json_delta":
                b["_json"] = b.get("_json", "") + d["partial_json"]
        elif e["type"] == "message_delta":
            stop = e["delta"].get("stop_reason") or stop
    for b in content:
        if "_json" in b:
            b["input"] = json.loads(b.pop("_json"))
    return {"id": "msg_mock", "type": "message", "role": "assistant", "model": "mock", "content": content,
            "stop_reason": stop, "usage": {"input_tokens": 50, "output_tokens": 40}}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send_json(self, obj):
        out = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def do_GET(self):  # model lookups: a 1M-token window for any model asked about
        def model(mid):
            return {"type": "model", "id": mid, "display_name": mid, "created_at": "2026-01-01T00:00:00Z",
                    "max_input_tokens": 1000000, "max_tokens": 128000}
        path = self.path.split("?")[0].rstrip("/")
        if "/models/" in path:
            return self.send_json(model(path.rsplit("/", 1)[1]))
        self.send_json({"data": [model("claude-opus-5-5")], "has_more": False, "first_id": None, "last_id": None})

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        body = json.loads(raw or b"{}")
        if self.path.split("?")[0].endswith("count_tokens"):
            return self.send_json({"input_tokens": len(raw) // 4})
        if LOG:
            with open(LOG, "a") as f:
                f.write(json.dumps(body) + "\n")
        done = sum(1 for m in body.get("messages", [])
                   if m["role"] == "user" and isinstance(m["content"], list)
                   and any(b.get("type") == "tool_result" for b in m["content"]))
        usage = {"input_tokens": 50, "cache_read_input_tokens": len(raw) // 4}
        if SCENARIO == "features":
            events = features(body, done, usage)
        elif SCENARIO == "memory":  # saves a note, then answers
            events = (reply([tool("m1", "memory", {"action": "add", "note": "uses pnpm, not npm"})], "tool_use", usage)
                      if done == 0 else text_reply("Noted.\n", usage))
        elif SCENARIO == "web":
            events = web(usage)
        elif SCENARIO == "bench":
            # Count results, not messages: some agents put several rounds'
            # results in one message.
            results = sum(1 for m in body.get("messages", []) if isinstance(m["content"], list)
                          for b in m["content"] if b.get("type") == "tool_result")
            events = bench(body, results, usage)
        else:
            events = tool_round(done, usage) if done < ROUNDS else final(usage)
        if body.get("stream") is not True:
            return self.send_json(as_message(events))
        parts = [sse([e]) for e in events]
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(sum(len(p) for p in parts)))
        self.end_headers()
        if DELAY_MS <= 0:
            return self.wfile.write(b"".join(parts))
        for part in parts:
            self.wfile.write(part)
            self.wfile.flush()
            time.sleep(DELAY_MS / 1000)

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
