# Resizing must leave exactly what a fresh run at the final size would show:
# nothing twice, nothing missing, the input box where it belongs. Runs the UI
# inside tmux (a real terminal with scrollback) against bench/mock.py, resizes
# it in bursts while a reply streams and again afterwards, and compares the
# whole scrollback with a run that was never resized.
#
#   python3 bench/resize.py target/release/claude-jr

import os, socket, subprocess, sys, tempfile, time

binary = os.path.realpath(sys.argv[1])
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FINAL = (100, 30)


def tmux(sock, *args):
    return subprocess.run(["tmux", "-L", sock, *args], capture_output=True, text=True).stdout


def capture(sock):
    lines = [l.rstrip() for l in tmux(sock, "capture-pane", "-p", "-t", "t", "-S", "-", "-E", "-").split("\n")]
    while lines and not lines[-1]:
        lines.pop()
    return lines


def settle(sock, quiet=1.5, limit=60):
    """Waits until the screen has stopped changing."""
    last, since, end = None, time.time(), time.time() + limit
    while time.time() < end:
        now = capture(sock)
        if now != last:
            last, since = now, time.time()
        elif time.time() - since > quiet:
            return now
        time.sleep(0.1)
    raise SystemExit("the screen never settled")


def run(layout, start, during, after):
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    mock = subprocess.Popen(
        [sys.executable, "-I", os.path.join(root, "bench/mock.py"), str(port)],
        env=dict(os.environ, MOCK_SCENARIO="bench", MOCK_ROUNDS="3", MOCK_TEXT_CHARS="1500",
                 MOCK_DELAY_MS="4", MOCK_OUTPUT_KB="1"),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    home = tempfile.mkdtemp()
    os.makedirs(f"{home}/claude-jr")
    open(f"{home}/claude-jr/config.json", "w").write('{"layout": "%s"}' % layout)
    conf = f"{home}/tmux.conf"
    open(conf, "w").write("set -g history-limit 50000\nset -g status off\n")
    sock = f"cjr-{os.getpid()}-{port}"
    env = (f"HOME={home} XDG_DATA_HOME={home} XDG_CONFIG_HOME={home} ANTHROPIC_API_KEY=mock "
           f"ANTHROPIC_BASE_URL=http://127.0.0.1:{port}")
    try:
        tmux(sock, "-f", conf, "new-session", "-d", "-s", "t", "-x", str(start[0]), "-y", str(start[1]),
             "-c", home, f"env {env} {binary} -y")
        settle(sock, quiet=0.7)
        tmux(sock, "send-keys", "-t", "t", "go", "Enter")
        for cols, rows, pause in during:
            time.sleep(pause)
            tmux(sock, "resize-window", "-t", "t", "-x", str(cols), "-y", str(rows))
        settle(sock)
        for cols, rows, pause in after:
            time.sleep(pause)
            tmux(sock, "resize-window", "-t", "t", "-x", str(cols), "-y", str(rows))
        return settle(sock)
    finally:
        tmux(sock, "kill-server")
        mock.kill()


def numberless(lines):
    """Timings and costs differ between runs, and so does the number of
    empty rows above the input box in the bottom layout (the live area was
    taller while working, and a resize lays everything out afresh)."""
    out = []
    for l in lines:
        l = "".join("#" if c.isdigit() else c for c in l)
        if l or (out and out[-1]):
            out.append(l)
    return out


# A zoom is a burst of resizes a few milliseconds apart.
BURST = [(110, 36, 0.4), (104, 33, 0.02), (96, 30, 0.02), (90, 27, 0.02), (84, 24, 0.3), (120, 40, 0.02)]
BACK = [(130, 44, 0.02), (70, 20, 0.02), (60, 18, 0.1), (FINAL[0], FINAL[1], 0.02)]

failed = False
for layout in ("bottom", "inline"):
    want = run(layout, FINAL, [], [])
    got = run(layout, (120, 40), BURST, BACK)
    headers = sum("/help for commands" in l for l in got)
    same = numberless(got) == numberless(want)
    print(f"{layout}: {len(got)} lines, header drawn {headers}x, same as a fresh run: {same}")
    if headers != 1 or not same:
        failed = True
        a, b = numberless(want), numberless(got)
        for i in range(max(len(a), len(b))):
            x, y = (a[i] if i < len(a) else None), (b[i] if i < len(b) else None)
            if x != y:
                print(f"  first difference at line {i + 1}; fresh / resized from there:")
                want, got = a, b
                for l in want[max(0, i - 2):i + 6]:
                    print(f"    fresh   | {l[:90]}")
                for l in got[max(0, i - 2):i + 6]:
                    print(f"    resized | {l[:90]}")
                break
sys.exit(1 if failed else 0)
