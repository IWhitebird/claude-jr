# Starts the interactive UI in a pseudo-terminal, waits, and reports startup
# time, memory and how much CPU it used while sitting idle.
#
#   python3 bench/idle.py target/release/claude-jr [seconds]

import os, pty, select, struct, sys, tempfile, time, fcntl, termios

binary = sys.argv[1]
idle_secs = float(sys.argv[2]) if len(sys.argv) > 2 else 5
data = tempfile.mkdtemp()

start = time.perf_counter()
pid, fd = pty.fork()
if pid == 0:
    env = dict(os.environ, ANTHROPIC_API_KEY="unused", TERM="xterm-256color", XDG_DATA_HOME=data)
    os.execve(binary, [binary], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))

first_frame, out = None, b""
deadline = time.time() + 3
while time.time() < deadline and first_frame is None:
    if select.select([fd], [], [], 0.01)[0]:
        chunk = os.read(fd, 65536)
        if b"\x1b[c" in chunk:
            os.write(fd, b"\x1b[?62c")  # answer the terminal query like a basic terminal
        out += chunk
        if "╭".encode() in out:
            first_frame = time.perf_counter() - start


def cpu_ticks():
    fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])


def status(key):
    for line in open(f"/proc/{pid}/status"):
        if line.startswith(key):
            return line.split()[1]


time.sleep(0.5)
before = cpu_ticks()
end = time.time() + idle_secs
while time.time() < end:  # keep draining the pty so the program never blocks
    if select.select([fd], [], [], 0.1)[0]:
        os.read(fd, 65536)
used_ms = (cpu_ticks() - before) * 1000 / os.sysconf("SC_CLK_TCK")

print(f"first frame       {first_frame * 1000:.1f} ms" if first_frame else "first frame       (not seen)")
print(f"memory, idle      {int(status('VmRSS:')) / 1024:.1f} MB")
print(f"threads           {status('Threads:')}")
print(f"CPU over {idle_secs:.0f}s idle  {used_ms:.0f} ms")
os.kill(pid, 9)
