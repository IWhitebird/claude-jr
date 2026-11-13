# Random keys, pastes and resizes against the real UI in tmux, with the fake
# API answering. Whatever is typed, the app must stay up, keep its header
# drawn exactly once, keep the input box on screen, never send the API a
# conversation it would refuse, and still take /exit. Seeds are printed, so a failure can be
# run again.
#
#   python3 bench/chaos.py target/release/claudejr            # 6 seeds
#   python3 bench/chaos.py target/release/claudejr 12 400     # 12 seeds, 400 actions each
#   SEED=7 python3 bench/chaos.py target/release/claudejr     # one seed
import os, random, shutil, socket, subprocess, sys, tempfile, time

binary = os.path.realpath(sys.argv[1])
seeds = int(sys.argv[2]) if len(sys.argv) > 2 else 6
actions = int(sys.argv[3]) if len(sys.argv) > 3 else 250
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

KEYS = ["Enter", "Escape", "Tab", "BTab", "Up", "Down", "Left", "Right", "Home", "End", "BSpace", "DC",
        "C-a", "C-e", "C-u", "C-k", "C-w", "C-s", "C-l", "C-v", "C-r", "C-o", "C-t", "C-j", "M-Enter",
        "PPage", "NPage", "Space", "y", "n", "a", "1", "2", "3"]
WORDS = ["hello", "go", "fix the bug", "/", "/help", "/model", "/settings", "/rewind", "/cost", "/stats",
         "/clear", "/compact", "/mcp", "/jobs", "/skills", "/sessions", "/rename chaos", "/mode", "/update x",
         "/nonsense", "!echo hi", "!", "@", "src/main.rs", "/tmp/x.png", "日本語", "👍🏽", "é", "\\", "`", "~",
         "a" * 300, "#note", "?"]
PASTES = ["one line", "line\n" * 9, "x" * 900, "tab\there", "\x1b[31mred\x1b[0m", "/clear"]


def tmux(sock, *args):
    return subprocess.run(["tmux", "-L", sock, *args], capture_output=True, text=True)


def screen(sock, whole=False):
    args = ["capture-pane", "-p", "-t", "t"] + (["-S", "-", "-E", "-"] if whole else [])
    return tmux(sock, *args).stdout


def wait(sock, what, limit=20):
    end = time.time() + limit
    while time.time() < end:
        if what(sock):
            return True
        time.sleep(0.1)
    return False


def alive(sock):
    return tmux(sock, "display", "-p", "-t", "t", "#{pane_dead}").stdout.strip() == "0"


def run(seed):
    rng = random.Random(seed)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    scenario = rng.choice(["default", "bench", "features", "web"])
    home = tempfile.mkdtemp()
    # Some requests go wrong along the way, as they do.
    kinds = ["500", "529", "429", "400", "cut", "early", "garbage", "overloaded", "half-tool"]
    fail = ",".join(f"{n}:{rng.choice(kinds)}" for n in sorted(rng.sample(range(1, 40), rng.randint(0, 8))))
    mock = subprocess.Popen(
        [sys.executable, "-I", os.path.join(root, "bench/mock.py"), str(port)],
        env=dict(os.environ, MOCK_LOG=f"{home}/requests", MOCK_SCENARIO=scenario, MOCK_FAIL=fail, MOCK_ROUNDS=str(rng.randint(1, 4)), MOCK_OUTPUT_KB="1",
                 MOCK_TEXT_CHARS="600", MOCK_DELAY_MS=str(rng.choice([0, 2, 8]))),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    os.makedirs(f"{home}/claude-jr")
    layout = rng.choice(["bottom", "inline"])
    open(f"{home}/claude-jr/config.json", "w").write('{"layout": "%s"}' % layout)
    conf = f"{home}/tmux.conf"
    open(conf, "w").write("set -g history-limit 50000\nset -g status off\nset -g remain-on-exit on\n")
    sock = f"chaos-{os.getpid()}-{seed}"
    mode = rng.choice(["-y", "-y", ""])  # without -y, approval prompts get the random keys too
    env = (f"HOME={home} XDG_DATA_HOME={home} XDG_CONFIG_HOME={home} ANTHROPIC_API_KEY=mock "
           f"ANTHROPIC_BASE_URL=http://127.0.0.1:{port} CLAUDE_JR_OFFLINE_SECS=3")
    log = []
    problem = None
    try:
        tmux(sock, "-f", conf, "new-session", "-d", "-s", "t", "-x", "100", "-y", "30", "-c", home,
             f"env {env} {binary} {mode}; echo; echo EXITED:$?")
        if not wait(sock, lambda s: "ask anything" in screen(s) or "trust" in screen(s).lower()):
            return f"never started ({mode})", log
        for i in range(actions):
            kind = rng.random()
            if kind < 0.45:
                act = ("key", rng.choice(KEYS))
                tmux(sock, "send-keys", "-t", "t", act[1])
            elif kind < 0.75:
                act = ("type", rng.choice(WORDS))
                tmux(sock, "send-keys", "-t", "t", "-l", act[1])
                if rng.random() < 0.6:
                    tmux(sock, "send-keys", "-t", "t", "Enter")
            elif kind < 0.85:
                act = ("paste", rng.choice(PASTES))
                tmux(sock, "set-buffer", act[1])
                tmux(sock, "paste-buffer", "-p", "-t", "t")
            else:
                act = ("resize", (rng.randint(8, 160), rng.randint(3, 50)))
                tmux(sock, "resize-window", "-t", "t", "-x", str(act[1][0]), "-y", str(act[1][1]))
            log.append(act)
            time.sleep(rng.choice([0, 0, 0.01, 0.05, 0.2]))
            if not alive(sock):
                out = screen(sock, True)
                # /exit and friends are fair; a crash is not.
                if "EXITED:0" in out and "panicked" not in out:
                    return None, log
                tail = "\n".join(out.rstrip().split("\n")[-12:])
                return f"the app died at action {i}:\n{tail}", log

        # Back to a normal size and out of whatever is open; then it must answer.
        tmux(sock, "resize-window", "-t", "t", "-x", "100", "-y", "30")
        for _ in range(4):
            tmux(sock, "send-keys", "-t", "t", "Escape")
            time.sleep(0.3)
        if not wait(sock, lambda s: not alive(s) or "esc to stop" not in screen(s), 60):
            return "still working a minute after Esc", log
        if alive(sock):
            # ctrl+c empties the input, however many lines are in it.
            for _ in range(3):
                if alive(sock) and "ask anything" not in screen(sock):
                    tmux(sock, "send-keys", "-t", "t", "C-c")
                    time.sleep(0.5)
        if alive(sock):
            time.sleep(1)
            now = screen(sock)
            if "╰" not in now or "╭" not in now:
                problem = "the input box is not on screen:\n" + now
            whole = screen(sock, True)
            headers = sum("/help for commands" in l for l in whole.split("\n"))
            if not problem and headers != 1:
                problem = f"the header is drawn {headers} times"
            tmux(sock, "send-keys", "-t", "t", "-l", "/exit")
            tmux(sock, "send-keys", "-t", "t", "Enter")
            if not wait(sock, lambda s: not alive(s), 15):
                return problem or "/exit did not close the app:\n" + screen(sock), log
        out = screen(sock, True)
        if "panicked" in out or "EXITED:0" not in out:
            return "it did not exit cleanly:\n" + "\n".join(out.rstrip().split("\n")[-10:]), log
        return problem, log
    finally:
        tmux(sock, "kill-server")
        mock.kill()
        refused = open(f"{home}/requests.invalid").read() if os.path.exists(f"{home}/requests.invalid") else ""
        shutil.rmtree(home, ignore_errors=True)
        if refused:
            # A conversation the real API would refuse is a bug whatever else happened.
            return "a malformed request was sent: " + refused.split("\n")[0], log


failed = 0
only = os.environ.get("SEED")
for seed in ([int(only)] if only else range(1, seeds + 1)):
    problem, log = run(seed)
    print(f"seed {seed}: {'ok' if not problem else 'FAILED'} ({len(log)} actions)", flush=True)
    if problem:
        failed += 1
        print("  " + problem.replace("\n", "\n  "))
        print("  last actions:", log[-12:])
sys.exit(1 if failed else 0)
