# Exit 0 if the task is done, 1 with a reason if not. Runs in the agent's working directory.
import os, subprocess, sys, tempfile

def node_test(code):
    """Runs hidden tests (an ES module) against the agent's files. Exit 0 = pass."""
    with tempfile.NamedTemporaryFile("w", suffix=".mjs", dir=".", delete=False) as f:
        f.write(code)
    try:
        r = subprocess.run(["node", f.name], capture_output=True, text=True, timeout=60)
    finally:
        os.unlink(f.name)
    print(r.stdout[-2000:], r.stderr[-2000:])
    sys.exit(r.returncode)

node_test("""
import assert from "node:assert/strict";
import { debounce } from "./debounce.js";
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let calls = [];
const d = debounce(function (x) { calls.push([this && this.tag, x]); }, 40);
const obj = { tag: "me", d };
obj.d(1); obj.d(2); obj.d(3);
assert.equal(calls.length, 0, "not called right away");
await wait(80);
assert.deepEqual(calls, [["me", 3]], "called once, with the last arguments and this");
d(4); await wait(20); d(5); await wait(20); d(6);
await wait(80);
assert.equal(calls.length, 2);
assert.equal(calls[1][1], 6);
d(7); d.cancel(); await wait(80);
assert.equal(calls.length, 2, "cancel drops the pending call");
console.log("ok");
""")
