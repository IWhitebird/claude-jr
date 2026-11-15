# A fake Messages API for tests and benchmarks. No network, no key.
#
# Each reply either calls tools or, once MOCK_ROUNDS rounds of tool results have
# come back, finishes with a short markdown answer. Every round runs a bash
# command printing MOCK_OUTPUT_KB of output, so the history grows like a real
# long session. The first round also writes and edits a file. MOCK_CMD
# replaces the bash command and MOCK_LOG records every request body (and the
# request's headers in MOCK_LOG.headers). POST .../chat/completions gets the
# same service in the Chat Completions format.
#
#   python3 bench/mock.py PORT      (0: any free port; the port is printed)

import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROUNDS = int(os.environ.get("MOCK_ROUNDS", "2"))
OUTPUT_KB = int(os.environ.get("MOCK_OUTPUT_KB", "1"))
LOG = os.environ.get("MOCK_LOG")
CMD = os.environ.get("MOCK_CMD")  # replaces the bash command of every round
SCENARIO = os.environ.get("MOCK_SCENARIO", "default")  # or "features", "bench", "web", "memory", "agents"
TEXT_CHARS = int(os.environ.get("MOCK_TEXT_CHARS", "2000"))  # "bench": streamed text per reply
DELAY_MS = float(os.environ.get("MOCK_DELAY_MS", "0"))  # pause between events, like a real stream
# Requests that go wrong, by position (1 = the first request): "2:500,3:cut".
# Kinds: a status code (400, 401, 429, 500, 529), cut (the stream stops
# half-way), early (it stops before any content), garbage (200 with a body
# that isn't a stream), overloaded (an error event in the stream), half-tool
# (the stream stops inside a tool call's input).
FAIL = dict((int(n), kind) for n, kind in
            (f.split(":") for f in os.environ.get("MOCK_FAIL", "").split(",") if f))
SEEN = [0]


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


def invalid(body):
    """Why the real API would refuse this request, or None. It checks what an
    agent can get wrong: who speaks when, and that every tool call is answered
    in the very next message and nothing else is."""
    messages = body.get("messages")
    if not isinstance(messages, list) or not messages:
        return "messages: at least one message is required"
    turns = []  # consecutive messages from one side count as one, as the API joins them
    for i, m in enumerate(messages):
        if not isinstance(m, dict) or m.get("role") not in ("user", "assistant"):
            return f"messages.{i}: role must be user or assistant"
        content = m.get("content")
        if isinstance(content, str):
            content = [{"type": "text", "text": content}]
        if not isinstance(content, list) or not content:
            return f"messages.{i}: content must not be empty"
        for j, b in enumerate(content):
            if not isinstance(b, dict) or "type" not in b:
                return f"messages.{i}.content.{j}: not a content block"
            if b["type"] == "text" and not str(b.get("text", "")).strip():
                return f"messages.{i}.content.{j}: text content blocks must contain non-whitespace text"
            if b["type"] == "tool_use" and m["role"] != "assistant":
                return f"messages.{i}.content.{j}: tool_use is only allowed in assistant messages"
            if b["type"] == "tool_result" and m["role"] != "user":
                return f"messages.{i}.content.{j}: tool_result is only allowed in user messages"
        if turns and turns[-1][0] == m["role"]:
            turns[-1][1].extend(content)
        else:
            turns.append((m["role"], list(content)))
    if turns[0][0] != "user":
        return "messages: the first message must use the user role"
    if turns[-1][0] != "user":
        return "messages: the conversation must end with a user message"
    asked = set()
    for role, content in turns:
        if role == "user":
            answered = [b.get("tool_use_id") for b in content if b["type"] == "tool_result"]
            if len(set(answered)) != len(answered):
                return "messages: a tool_use id was answered twice"
            if set(answered) != asked:
                missing, extra = asked - set(answered), set(answered) - asked
                return (f"messages: tool_use ids without a tool_result right after: {sorted(missing)}" if missing
                        else f"messages: unexpected tool_result ids: {sorted(extra)}")
            asked = set()
        else:
            ids = [b.get("id") for b in content if b["type"] == "tool_use"]
            if len(set(ids)) != len(ids):
                return "messages: tool_use ids must be unique"
            asked = set(ids)
    return None


# ---- the same service in the Chat Completions format --------------------
# A request in that format is turned into the Messages format, answered by
# the scenarios above, and the answer turned back. What the real services
# refuse is refused here too.

def from_chat(body):
    """A Chat Completions request as a Messages request, or (None, why) if a
    real server would refuse it."""
    out, system = [], []
    for i, m in enumerate(body.get("messages") or []):
        role, content = m.get("role"), m.get("content")
        if role == "system":
            if i != 0:
                return None, "messages: the system message must come first"
            system.append(content if isinstance(content, str) else "")
        elif role == "user":
            blocks = [{"type": "text", "text": content}] if isinstance(content, str) else []
            for part in content if isinstance(content, list) else []:
                if part.get("type") == "text":
                    blocks.append({"type": "text", "text": part.get("text", "")})
                elif part.get("type") == "image_url":
                    url = part.get("image_url", {}).get("url", "")
                    if not url.startswith("data:image/") or ";base64," not in url:
                        return None, f"messages.{i}: image_url must be a data: URL here"
                    blocks.append({"type": "text", "text": "[image]"})
                else:
                    return None, f"messages.{i}: unknown content part {part.get('type')!r}"
            out.append({"role": "user", "content": blocks})
        elif role == "tool":
            if not isinstance(content, str):
                return None, f"messages.{i}: a tool message's content must be a string"
            block = {"type": "tool_result", "tool_use_id": m.get("tool_call_id"), "content": content}
            # Results for one reply's calls arrive as separate messages.
            if out and out[-1]["role"] == "user" and all(b["type"] == "tool_result" for b in out[-1]["content"]):
                out[-1]["content"].append(block)
            else:
                out.append({"role": "user", "content": [block]})
        elif role == "assistant":
            blocks = [{"type": "text", "text": content}] if isinstance(content, str) and content.strip() else []
            for call in m.get("tool_calls") or []:
                fn = call.get("function", {})
                if call.get("type") != "function" or not isinstance(fn.get("arguments"), str):
                    return None, f"messages.{i}: tool_calls need type function and arguments as a JSON string"
                try:
                    args = json.loads(fn["arguments"])
                except ValueError:
                    return None, f"messages.{i}: tool call arguments are not valid JSON"
                blocks.append({"type": "tool_use", "id": call.get("id"), "name": fn.get("name"), "input": args})
            if not blocks:
                blocks = [{"type": "text", "text": "(nothing)"}]
            out.append({"role": "assistant", "content": blocks})
        else:
            return None, f"messages.{i}: unknown role {role!r}"
    tools = []
    for t in body.get("tools") or []:
        fn = t.get("function", {})
        if t.get("type") != "function" or not fn.get("name") or not isinstance(fn.get("parameters"), dict):
            return None, "tools: each needs type function, a name and parameters"
        tools.append({"name": fn["name"], "description": fn.get("description", ""), "input_schema": fn["parameters"]})
    for field in ("thinking", "cache_control", "context_management", "output_config", "system", "max_tokens_to_sample"):
        if field in body:
            return None, f"unknown field: {field}"
    return {"messages": out, "system": "\n".join(system), "tools": tools, "model": body.get("model"),
            "stream": body.get("stream")}, None


def to_chat(events):
    """Messages stream events as Chat Completions chunks."""
    chunks, calls, stop, usage = [], -1, "stop", {}
    def chunk(delta, finish=None):
        return {"id": "c1", "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
    for e in events:
        t = e["type"]
        if t == "message_start":
            usage = dict(e["message"]["usage"])
            chunks.append(chunk({"role": "assistant", "content": ""}))
        elif t == "content_block_start" and e["content_block"]["type"] == "tool_use":
            calls += 1
            b = e["content_block"]
            chunks.append(chunk({"tool_calls": [{"index": calls, "id": b["id"], "type": "function",
                                                 "function": {"name": b["name"], "arguments": ""}}]}))
        elif t == "content_block_delta":
            d = e["delta"]
            if d["type"] == "text_delta":
                chunks.append(chunk({"content": d["text"]}))
            elif d["type"] == "thinking_delta":
                chunks.append(chunk({"reasoning_content": d["thinking"]}))
            elif d["type"] == "input_json_delta":
                chunks.append(chunk({"tool_calls": [{"index": calls, "function": {"arguments": d["partial_json"]}}]}))
        elif t == "message_delta":
            stop = {"tool_use": "tool_calls", "max_tokens": "length"}.get(e["delta"].get("stop_reason"), "stop")
            usage["output_tokens"] = e.get("usage", {}).get("output_tokens", 0)
        elif t == "error":
            chunks.append({"error": e["error"]})
    cached = usage.get("cache_read_input_tokens", 0)
    chunks.append(chunk({}, stop))
    chunks.append({"id": "c1", "object": "chat.completion.chunk", "choices": [],
                   "usage": {"prompt_tokens": usage.get("input_tokens", 0) + cached,
                             "completion_tokens": usage.get("output_tokens", 0),
                             "prompt_tokens_details": {"cached_tokens": cached}}})
    return chunks


def chat_sse(chunks, done=True):
    return ("".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + ("data: [DONE]\n\n" if done else "")).encode()


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
            with open(LOG + ".headers", "a") as f:
                f.write(json.dumps(dict({k.lower(): v for k, v in self.headers.items()}, path=self.path)) + "\n")
        chat = self.path.split("?")[0].endswith("/chat/completions")
        why = None
        if chat:
            body, why = from_chat(body)
        elif self.headers.get("anthropic-version") is None:
            why = "anthropic-version: header is required"
        why = why or invalid(body)
        if why:
            out = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": why}}).encode()
            if LOG:
                with open(LOG + ".invalid", "a") as f:
                    f.write(why + "\n")
            self.send_response(400)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(out)))
            self.end_headers()
            return self.wfile.write(out)
        done = sum(1 for m in body.get("messages", [])
                   if m["role"] == "user" and isinstance(m["content"], list)
                   and any(b.get("type") == "tool_result" for b in m["content"]))
        usage = {"input_tokens": 50, "cache_read_input_tokens": len(raw) // 4}
        if SCENARIO == "features":
            events = features(body, done, usage)
        elif SCENARIO == "memory":  # saves a note, then answers
            events = (reply([tool("m1", "memory", {"action": "add", "note": "uses pnpm, not npm"})], "tool_use", usage)
                      if done == 0 else text_reply("Noted.\n", usage))
        elif SCENARIO == "agents":  # two slow subagents, then a server left running
            sub = "You are a subagent" in body.get("system", "")
            if sub:
                events = (reply([tool("s1", "bash", {"command": "sleep 1.5; echo looked"})], "tool_use", usage)
                          if done == 0 else text_reply("Found it.", usage))
            elif done == 0:
                events = reply([tool("a1", "agent", {"description": "scan components", "prompt": "x"}),
                                tool("a2", "agent", {"description": "check styles", "prompt": "y"})], "tool_use", usage)
            elif done == 1:
                events = reply([tool("b1", "bash", {"command": "sleep 60", "background": True})], "tool_use", usage)
            else:
                events = text_reply("Started the dev server.\n", usage)
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
        SEEN[0] += 1
        kind = FAIL.get(SEEN[0])
        if kind and kind.isdigit():
            names = {"400": "invalid_request_error", "401": "authentication_error", "429": "rate_limit_error",
                     "529": "overloaded_error"}
            out = json.dumps({"type": "error", "error": {"type": names.get(kind, "api_error"),
                                                         "message": f"the fake API was told to answer {kind}"}}).encode()
            self.send_response(int(kind))
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(out)))
            if kind == "429":
                self.send_header("retry-after", "0")
            self.end_headers()
            return self.wfile.write(out)
        if kind == "garbage":
            out = b"<html>502 from some proxy</html>"
            self.send_response(200)
            self.send_header("content-type", "text/html")
            self.send_header("content-length", str(len(out)))
            self.end_headers()
            return self.wfile.write(out)
        if chat and kind not in ("garbage",) and not (kind and kind.isdigit()):
            return self.send_chat(events, kind)
        if kind in ("cut", "early", "overloaded", "half-tool"):
            if kind == "early":
                events = events[:1]
            elif kind == "half-tool":
                at = next((i for i, e in enumerate(events) if e.get("delta", {}).get("type") == "input_json_delta"),
                          len(events) // 2)
                events = events[:at + 1]
            else:
                events = events[:max(2, len(events) // 2)]
            if kind == "overloaded":
                events.append({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("connection", "close")
            self.end_headers()
            self.wfile.write(sse(events))
            self.wfile.flush()
            self.close_connection = True
            return
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

    def send_chat(self, events, kind):
        """The reply in the Chat Completions format, whole or cut as `kind` says."""
        done = True
        if kind == "early":
            events, done = events[:1], False
        elif kind == "half-tool":
            at = next((i for i, e in enumerate(events) if e.get("delta", {}).get("type") == "input_json_delta"),
                      len(events) // 2)
            events, done = events[:at + 1], False
        elif kind in ("cut", "overloaded"):
            events, done = events[:max(2, len(events) // 2)], False
            if kind == "overloaded":
                events.append({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
        chunks = to_chat(events)
        if not done:
            # A stream that stops has no closing chunks either.
            chunks = [c for c in chunks if c.get("choices") and c["choices"][0]["finish_reason"] is None
                      or "error" in c]
        parts = [chat_sse([c], False) for c in chunks] + ([b"data: [DONE]\n\n"] if done else [])
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        if not done:
            self.send_header("connection", "close")
            self.end_headers()
            self.wfile.write(b"".join(parts))
            self.wfile.flush()
            self.close_connection = True
            return
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


server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler)
# With port 0 the system picks a free one; either way, say which it is.
print(server.server_address[1], flush=True)
server.serve_forever()
