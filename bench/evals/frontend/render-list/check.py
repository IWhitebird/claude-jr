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
import { renderTodos as r } from "./todos.js";
const norm = (s) => s.replace(/>\\s+</g, "><").trim();
assert.equal(norm(r([])), '<p class="empty">Nothing to do</p>');
const html = norm(r([{ title: "Buy milk", done: false }, { title: "Ship it", done: true }]));
assert.match(html, /^<ul class="todos">/);
assert.match(html, /<li>Buy milk<\\/li>/);
assert.match(html, /<li class="done">Ship it<\\/li>/);
const evil = norm(r([{ title: '<img src=x onerror="alert(1)">&', done: false }]));
assert.ok(!evil.includes("<img"), "titles must be escaped");
assert.ok(evil.includes("&lt;img") && evil.includes("&amp;") && evil.includes("&quot;"), "escape < > & and quotes");
console.log("ok");
""")
