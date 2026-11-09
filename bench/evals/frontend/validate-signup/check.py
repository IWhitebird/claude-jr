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
import { validateSignup as v } from "./validate.js";
assert.deepEqual(v({ email: "a@b.co", password: "hunter22", confirm: "hunter22" }), {});
assert.deepEqual(v({ email: "  a@b.co ", password: "hunter22", confirm: "hunter22" }), {});
assert.deepEqual(v({ email: "", password: "hunter22", confirm: "hunter22" }), { email: "Email is required" });
for (const bad of ["ab.co", "a@b", "@b.co", "a@@b.co", "a@b."]) {
  assert.equal(v({ email: bad, password: "hunter22", confirm: "hunter22" }).email, "Enter a valid email", bad);
}
assert.deepEqual(v({ email: "a@b.co", password: "short1", confirm: "short1" }), { password: "At least 8 characters" });
assert.deepEqual(v({ email: "a@b.co", password: "longenough", confirm: "longenough" }), { password: "Include a number" });
assert.deepEqual(v({ email: "a@b.co", password: "hunter22", confirm: "hunter23" }), { confirm: "Passwords don't match" });
console.log("ok");
""")
