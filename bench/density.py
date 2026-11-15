# How many agents fit: runs that many headless agents at once against the
# fake API, each in its own folder, and samples what all of them together
# hold in memory (shared pages counted once; the commands they run aren't
# counted). Raise the open-file limit first for a thousand: ulimit -n 8192
#
#   python3 bench/density.py target/release/claudejr 1000
import os, shutil, socket, subprocess, sys, tempfile, time
B, N = os.path.realpath(sys.argv[1]), int(sys.argv[2])
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
with socket.socket() as s:
    s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]
mock = subprocess.Popen([sys.executable, "-I", f"{root}/bench/mock.py", str(port)],
                        env=dict(os.environ, MOCK_ROUNDS="6", MOCK_OUTPUT_KB="8", MOCK_DELAY_MS="60"),
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(0.6)
W = tempfile.mkdtemp()
env = {"PATH": os.environ["PATH"], "HOME": W, "XDG_DATA_HOME": W, "XDG_CONFIG_HOME": W, "ANTHROPIC_API_KEY": "mock",
       "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}", "CLAUDE_JR_OFFLINE_SECS": "60"}
t0 = time.time(); procs = []
for i in range(N):
    d = f"{W}/a{i}"; os.mkdir(d)
    procs.append(subprocess.Popen([B, "-y", "-p", "go", "--output-format", "json"], cwd=d, env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL))
pids = {p.pid for p in procs}
def sample():
    pss = rss = alive = 0
    for pid in pids:
        try:
            for line in open(f"/proc/{pid}/smaps_rollup"):
                if line.startswith("Pss:"): pss += int(line.split()[1])
                elif line.startswith("Rss:"): rss += int(line.split()[1])
            alive += 1
        except OSError: pass
    return alive, pss, rss
peak = (0, 0, 0)
while any(p.poll() is None for p in procs):
    s = sample()
    if s[0] >= N * 0.9 and s[1] > peak[1]: peak = s
    time.sleep(0.05)
ok = sum(1 for p in procs if p.returncode == 0 and b'"ok":true' in p.stdout.read())
cpu = os.times()
print(f"{N} agents: {ok} finished ok in {time.time()-t0:.1f}s; with {peak[0]} alive they held {peak[1]/1024:.0f} MB "
      f"in total ({peak[1]/max(peak[0],1):.0f} kB each, shared pages counted once); CPU for all agents and their commands {cpu.children_user+cpu.children_system:.1f}s")
mock.kill()
shutil.rmtree(W, ignore_errors=True)
sys.exit(0 if ok == N else 1)
