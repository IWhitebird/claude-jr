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
import { relativeTime as r } from "./time.js";
const now = new Date("2026-03-10T12:00:00Z");
const ago = (s) => new Date(now - s * 1000);
const ahead = (s) => new Date(+now + s * 1000);
assert.equal(r(ago(5), now), "just now");
assert.equal(r(ahead(30), now), "just now");
assert.equal(r(ago(60), now), "1 minute ago");
assert.equal(r(ago(125), now), "2 minutes ago");
assert.equal(r(ahead(600), now), "in 10 minutes");
assert.equal(r(ago(3600), now), "1 hour ago");
assert.equal(r(ago(5 * 3600 + 59), now), "5 hours ago");
assert.equal(r(ahead(3 * 3600), now), "in 3 hours");
assert.equal(r(ago(26 * 3600), now), "yesterday");
assert.equal(r(ahead(30 * 3600), now), "tomorrow");
assert.equal(r(ago(3 * 86400), now), "3 days ago");
assert.equal(r(ahead(5 * 86400), now), "in 5 days");
assert.equal(r(ago(10 * 86400), now), "2026-02-28");
console.log("ok");
""")
