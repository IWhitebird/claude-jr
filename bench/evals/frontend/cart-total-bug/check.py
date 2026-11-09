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

import hashlib
EXPECTED = "d58cbcb0b58d6ea0fb91c1f7f6aeff63482928c5716245009e860d2a8b134bfb"
need_hash = hashlib.sha256(open("cart.test.js", "rb").read()).hexdigest()
if need_hash != EXPECTED:
    print("FAIL: cart.test.js was changed"); sys.exit(1)
node_test(open("cart.test.js").read())
