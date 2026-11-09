# CPU and memory of the interactive UI while a reply streams in at a
# realistic pace (MOCK_DELAY_MS between events), against bench/mock.py.
#
#   python3 bench/ui.py target/release/claude-jr
#   ROUNDS=10 TEXT=8000 DELAY=15 python3 bench/ui.py target/release/claude-jr

import os, pty, select, socket, struct, subprocess, sys, tempfile, time, fcntl, termios

binary = os.path.realpath(sys.argv[1])
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
rounds, text, delay = os.environ.get("ROUNDS", "5"), os.environ.get("TEXT", "6000"), os.environ.get("DELAY", "15")

with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
mock = subprocess.Popen(
    [sys.executable, "-I", os.path.join(root, "bench/mock.py"), str(port)],
    env=dict(os.environ, MOCK_SCENARIO="bench", MOCK_ROUNDS=rounds, MOCK_TEXT_CHARS=text,
             MOCK_DELAY_MS=delay, MOCK_OUTPUT_KB="4"),
)
time.sleep(0.5)
home = tempfile.mkdtemp()

pid, fd = pty.fork()
if pid == 0:
    os.chdir(home)
    env = dict(os.environ, ANTHROPIC_API_KEY="mock", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}",
               TERM="xterm-256color", HOME=home, XDG_DATA_HOME=home, XDG_CONFIG_HOME=home)
    os.execve(binary, [binary, "-y"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))


def drain(seconds, until=None):
    out, end = b"", time.time() + seconds
    while time.time() < end:
        if select.select([fd], [], [], 0.02)[0]:
            chunk = os.read(fd, 65536)
            if b"\x1b[c" in chunk:
                os.write(fd, b"\x1b[?62c")
            if b"\x1b[6n" in chunk:  # cursor position, asked for by the bottom layout
                os.write(fd, b"\x1b[1;1R")
            out += chunk
            written[0] += len(chunk)
            if until and until in out[-4096:]:
                return True
    return False


def cpu():
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) * 1000 / os.sysconf("SC_CLK_TCK")


written = [0]
drain(1.0)
before, start, written[0] = cpu(), time.time(), 0
os.write(fd, b"go\r")
finished = drain(120, until="ask anything".encode())
wall = time.time() - start
used = cpu() - before
status = {l.split(":")[0]: l.split()[1] for l in open(f"/proc/{pid}/status") if len(l.split()) > 1}
streamed_kb = int(rounds) * int(text) / 1024 + int(text) / 1024

print(f"streamed          {streamed_kb:.0f} KB of text over {wall:.1f}s ({'finished' if finished else 'TIMED OUT'})")
print(f"CPU               {used:.0f} ms ({used / wall / 10:.2f}% of one core)")
print(f"CPU per KB        {used / streamed_kb:.2f} ms")
print(f"terminal output   {written[0] / 1024:.0f} KB")
print(f"peak memory       {int(status['VmHWM']) / 1024:.1f} MB")
for tid in sorted(os.listdir(f"/proc/{pid}/task")):
    name = open(f"/proc/{pid}/task/{tid}/comm").read().strip()
    f = open(f"/proc/{pid}/task/{tid}/stat").read().rsplit(")", 1)[1].split()
    ms = (int(f[11]) + int(f[12])) * 1000 / os.sysconf("SC_CLK_TCK")
    print(f"  thread {name:<12} {ms:.0f} ms")
os.kill(pid, 9)
mock.kill()
