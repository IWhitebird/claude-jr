# A fake Messages API for tests and benchmarks. No network, no key.
#
# Each reply either calls tools or, once MOCK_ROUNDS rounds of tool results have
# come back, finishes with a short markdown answer. Every round runs a bash
# command printing MOCK_OUTPUT_KB of output, so the history grows like a real
# long session. The first round also writes and edits a file. MOCK_CMD
# replaces the bash command and MOCK_LOG records every request body.
#
#   python3 bench/mock.py PORT

import json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROUNDS = int(os.environ.get("MOCK_ROUNDS", "2"))
OUTPUT_KB = int(os.environ.get("MOCK_OUTPUT_KB", "1"))
LOG = os.environ.get("MOCK_LOG")
CMD = os.environ.get("MOCK_CMD")  # replaces the bash command of every round


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
            "```sh\ncat demo.txt\n```\n")
    return reply([({"type": "text", "text": ""}, [{"type": "text_delta", "text": text[i:i + 20]}
                                                  for i in range(0, len(text), 20)])], "end_turn", usage)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["content-length"]))
        body = json.loads(raw)
        if LOG:
            with open(LOG, "a") as f:
                f.write(json.dumps(body) + "\n")
        done = sum(1 for m in body["messages"]
                   if m["role"] == "user" and isinstance(m["content"], list)
                   and any(b.get("type") == "tool_result" for b in m["content"]))
        usage = {"input_tokens": 50, "cache_read_input_tokens": len(raw) // 4}
        out = sse(tool_round(done, usage) if done < ROUNDS else final(usage))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
