#!/usr/bin/env python3
# Makes the recording at the top of the README and the site: a real claude-jr
# session in a real terminal (tmux), captured screen by screen and drawn as an
# animated SVG. Nothing in it is drawn or typed in afterwards. A wait longer
# than MAX_GAP would be cut down to it, and the render says when that
# happened; the take in the repository has none and plays in real time.
#
#   python3 scripts/demo.py record demo.json            # the real API: a few cents
#   python3 scripts/demo.py record demo.json --mock     # the fake API, to rehearse
#   python3 scripts/demo.py render demo.json assets/demo.svg
#   python3 scripts/demo.py render demo.json site/demo.svg --once   # plays once and stops
#   python3 scripts/demo.py render demo.json still.svg --at 12.5    # one moment, for checking
#
# The session: a shell in a small Python project with a failing test,
# `claudejr`, one request (a command is allowed for the session, an edit is
# approved), then /stats. It runs in a home of its own, which bubblewrap puts
# at /home/dev with the real /home out of sight, so no path, note or setting
# of yours can appear in it. The key is the one /login saved, or
# ANTHROPIC_API_KEY. Needs tmux and bwrap; BIN names the binary (default: the
# static release build).

import html, json, os, re, shutil, subprocess, sys, tempfile, threading, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
COLS, ROWS = 88, 27
PROMPT = "the slugify tests are failing, fix it"
MAX_GAP = 3.0      # seconds: the longest a wait is shown for
MIN_STEP = 0.07    # screens closer together than this are merged
LAST = 6.0         # how long the final screen stays before it starts again

FILES = {
    "AGENTS.md": "# slug\n\nTurns titles into URL slugs. Run the tests with `python3 -m unittest -q`.\n",
    "slugify.py": '''import re


def slugify(title):
    """The part of a URL that names a page: "Hello, World!" -> "hello-world"."""
    return title.lower().replace(" ", "-")
''',
    "test_slugify.py": '''import unittest

from slugify import slugify


class SlugifyTest(unittest.TestCase):
    def test_spaces(self):
        self.assertEqual(slugify("Hello World"), "hello-world")

    def test_punctuation(self):
        self.assertEqual(slugify("Hello, World!"), "hello-world")

    def test_extra_spaces(self):
        self.assertEqual(slugify("  Hello   World  "), "hello-world")


if __name__ == "__main__":
    unittest.main()
''',
}


# ---- recording ----

def record(out, mock):
    binary = os.path.abspath(os.environ.get("BIN", f"{ROOT}/target/x86_64-unknown-linux-musl/release/claudejr"))
    home = tempfile.mkdtemp(prefix="cjr-demo-")
    proj = f"{home}/proj/slug"
    os.makedirs(proj)
    for name, text in FILES.items():
        with open(f"{proj}/{name}", "w") as f:
            f.write(text)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ANTHROPIC_", "CLAUDE_JR_", "TMUX", "XDG_"))}
    env.update(HOME=home, TERM="xterm-256color", COLORTERM="truecolor")
    server = None
    if mock:
        server = subprocess.Popen([sys.executable, "-I", f"{ROOT}/bench/mock.py", "0"], stdout=subprocess.PIPE, text=True,
                                  env={**os.environ, "MOCK_DELAY_MS": "25", "MOCK_CMD": "python3 -m unittest 2>&1 | tail -4"})
        env.update(ANTHROPIC_API_KEY="mock", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{server.stdout.readline().strip()}")
    elif os.environ.get("ANTHROPIC_API_KEY"):
        env["ANTHROPIC_API_KEY"] = os.environ["ANTHROPIC_API_KEY"]
    else:
        # The key /login saved, copied into the scratch home (which is deleted at the end).
        saved = os.path.join(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")), "claude-jr/credentials")
        os.makedirs(f"{home}/.config/claude-jr", mode=0o700)
        shutil.copy(saved, f"{home}/.config/claude-jr/credentials")
        os.chmod(f"{home}/.config/claude-jr/credentials", 0o600)

    # A tmux server of its own: its sessions get this environment, not yours.
    tmux = ["tmux", "-L", f"cjr-demo-{os.getpid()}", "-f", "/dev/null"]
    run = lambda *a: subprocess.run(tmux + list(a), env=env, capture_output=True, text=True).stdout
    inside = "/home/dev"
    run("new-session", "-d", "-s", "demo", "-x", str(COLS), "-y", str(ROWS),
        "bwrap", "--dev-bind", "/", "/", "--tmpfs", "/home", "--bind", home, inside,
        "--ro-bind", binary, f"{inside}/.local/bin/claudejr", "--setenv", "HOME", inside,
        "--setenv", "PATH", f"{inside}/.local/bin:{env['PATH']}", "--setenv", "PS1", "\\[\\e[2m\\]~/proj/slug\\[\\e[0m\\] $ ",
        "--chdir", f"{inside}/proj/slug", "bash", "--norc", "--noprofile")
    run("set", "-g", "status", "off")

    frames, stop, start = [], threading.Event(), time.time()
    def watch():
        last = None
        while not stop.is_set():
            got = run("capture-pane", "-e", "-p", "-t", "demo", ";",
                      "display-message", "-p", "-t", "demo", "#{cursor_x} #{cursor_y} #{cursor_flag}")
            if got and got != last:
                screen, _, cursor = got.rstrip("\n").rpartition("\n")
                frames.append({"t": round(time.time() - start, 3), "screen": screen, "cursor": [int(n) for n in cursor.split()]})
                last = got
            time.sleep(0.025)
    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()

    plain = lambda: run("capture-pane", "-p", "-t", "demo")
    def wait(text, seconds, gone=False):
        end = time.time() + seconds
        while (text in plain()) == gone:
            if time.time() > end:
                raise SystemExit(f"timed out waiting for {text!r}:\n{plain()}")
            time.sleep(0.05)
    def type_(text):
        for ch in text:
            run("send-keys", "-t", "demo", "-l", ch)
            time.sleep(0.055 if ch != " " else 0.09)

    try:
        wait("slug $", 15)
        time.sleep(1.0)
        type_("claudejr")
        time.sleep(0.4)
        run("send-keys", "-t", "demo", "Enter")
        wait("ask anything", 15)
        time.sleep(1.4)
        type_(PROMPT)
        time.sleep(0.4)
        run("send-keys", "-t", "demo", "Enter")
        wait("esc to stop", 15)
        # Answer what it asks: commands always, each edit once. Done when the input box is back.
        quiet, end = 0.0, time.time() + 300
        while quiet < 1.5:
            if time.time() > end:
                raise SystemExit(f"the session did not finish:\n{plain()}")
            screen = plain()
            if "Allow Bash(" in screen:
                time.sleep(0.7); run("send-keys", "-t", "demo", "a"); time.sleep(0.3)
            elif "Allow " in screen and "y yes" in screen:
                time.sleep(0.7); run("send-keys", "-t", "demo", "y"); time.sleep(0.3)
            quiet = quiet + 0.1 if "esc to stop" not in screen and "ask anything" in screen else 0.0
            time.sleep(0.1)
        time.sleep(1.0)
        type_("/stats")
        time.sleep(0.5)
        run("send-keys", "-t", "demo", "Enter")
        wait("memory (peak", 10)
        time.sleep(1.0)
    finally:
        stop.set()
        watcher.join()
        run("kill-server")
        if server:
            server.terminate()
        shutil.rmtree(home, ignore_errors=True)

    version = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    with open(out, "w") as f:
        json.dump({"cols": COLS, "rows": ROWS, "version": version, "mock": mock, "prompt": PROMPT,
                   "recorded": time.strftime("%Y-%m-%d"), "seconds": frames[-1]["t"], "frames": frames}, f)
    print(f"{out}: {len(frames)} screens over {frames[-1]['t']:.1f}s ({version}{', fake API' if mock else ''})")


# ---- reading a captured screen ----

BASIC = ["#3b3632", "#e5786d", "#98c379", "#e5c07b", "#6fa8dc", "#c594c5", "#6cc5bf", "#d6d0c4",
         "#7d766c", "#f08c7e", "#b0d68e", "#f0d08f", "#8cbcea", "#d7a8d7", "#8ad9d3", "#f4f0e8"]
FG, BG = "#e8e3d9", "#100e0d"


def colour(codes, i):
    """The colour an SGR 38/48 sequence names, and how many numbers it took."""
    if codes[i + 1:i + 2] == [5] and i + 2 < len(codes):
        n = codes[i + 2]
        if n < 16:
            return BASIC[n], 3
        if n < 232:
            n -= 16
            return "#%02x%02x%02x" % tuple(0 if v == 0 else 55 + 40 * v for v in (n // 36, n // 6 % 6, n % 6)), 3
        return "#%02x%02x%02x" % ((8 + 10 * (n - 232),) * 3), 3
    if codes[i + 1:i + 2] == [2] and i + 4 < len(codes):
        return "#%02x%02x%02x" % tuple(codes[i + 2:i + 5]), 5
    return None, 1


def cells(line):
    """One screen line as (character, style) pairs; style is (fg, bg, bold, dim, italic, underline)."""
    out, fg, bg, bold, dim, italic, under, rev = [], None, None, False, False, False, False, False
    for part in re.split(r"(\x1b\[[0-9;]*m)", line):
        if not part.startswith("\x1b["):
            style = (bg if rev else fg, fg or FG if rev else bg, bold, dim, italic, under)
            out += [(ch, style) for ch in part]
            continue
        codes, i = [int(n or 0) for n in part[2:-1].split(";")], 0
        while i < len(codes):
            c, used = codes[i], 1
            if c == 0: fg, bg, bold, dim, italic, under, rev = None, None, False, False, False, False, False
            elif c == 1: bold = True
            elif c == 2: dim = True
            elif c == 3: italic = True
            elif c == 4: under = True
            elif c == 7: rev = True
            elif c == 22: bold = dim = False
            elif c == 23: italic = False
            elif c == 24: under = False
            elif c == 27: rev = False
            elif 30 <= c <= 37: fg = BASIC[c - 30]
            elif 90 <= c <= 97: fg = BASIC[c - 82]
            elif c == 39: fg = None
            elif 40 <= c <= 47: bg = BASIC[c - 40]
            elif 100 <= c <= 107: bg = BASIC[c - 92]
            elif c == 49: bg = None
            elif c == 38: fg, used = colour(codes, i)
            elif c == 48: bg, used = colour(codes, i)
            i += used
    while out and out[-1][0] == " " and out[-1][1][1] is None:
        out.pop()
    return tuple(out)


# ---- drawing ----

CW, CH, PAD, SIZE = 8.4, 19, 18, 14   # a cell's width and height, the margin, the font size
# Lines of the boxes and tables, drawn rather than left to whichever font is there:
# which of left, right, up, down each one reaches.
LINES = {"─": "lr", "│": "ud", "╭": "rd", "╮": "ld", "╰": "ru", "╯": "lu", "┌": "rd", "┐": "ld", "└": "ru",
         "┘": "lu", "├": "udr", "┤": "udl", "┬": "lrd", "┴": "lru", "┼": "lrud"}
BLOCKS = {"█": (0, 1), "▀": (0, 0.5), "▄": (0.5, 1)}   # the part of the cell's height that is filled


def n(v):
    return f"{v:.1f}".rstrip("0").rstrip(".")


def row_svg(row):
    """One line of the screen: backgrounds, then drawn characters, then text in runs of one style."""
    back, drawn, text, i = [], [], [], 0
    while i < len(row):
        j = i
        while j < len(row) and row[j][1] == row[i][1] and (row[j][0] in LINES or row[j][0] in BLOCKS) == (row[i][0] in LINES or row[i][0] in BLOCKS):
            j += 1
        (fg, bg, bold, dim, italic, under), x = row[i][1], i * CW
        chars = "".join(c for c, _ in row[i:j])
        if bg:
            back.append(f'<rect x="{n(x)}" width="{n((j - i) * CW + 0.3)}" height="{CH}" fill="{bg}"/>')
        ink = fg or FG
        if chars[0] in BLOCKS:
            for k, ch in enumerate(chars):
                a, b = BLOCKS[ch]
                drawn.append(f'<rect x="{n(x + k * CW)}" y="{n(a * CH)}" width="{n(CW + 0.3)}" height="{n((b - a) * CH + 0.3)}" fill="{ink}"/>')
        elif chars[0] in LINES:
            d, mid = "", CH / 2
            for k, ch in enumerate(chars):
                cx, reach = x + k * CW + CW / 2, LINES[ch]
                if ch in "╭╮╰╯":   # a quarter circle between the two arms
                    ex = cx + (CW / 2 if "r" in reach else -CW / 2)
                    ey = CH if "d" in reach else 0
                    d += f"M{n(ex)} {n(mid)}Q{n(cx)} {n(mid)} {n(cx)} {n(ey)}"
                    continue
                if "l" in reach or "r" in reach:
                    d += f"M{n(cx - CW / 2 if 'l' in reach else cx)} {n(mid)}H{n(cx + CW / 2 if 'r' in reach else cx)}"
                if "u" in reach or "d" in reach:
                    d += f"M{n(cx)} {n(0 if 'u' in reach else mid)}V{n(CH if 'd' in reach else mid)}"
            drawn.append(f'<path d="{d}" stroke="{ink}"{" opacity=\".5\"" if dim else ""} fill="none"/>')
        elif chars.strip():
            lead = len(chars) - len(chars.lstrip(" "))
            chars = chars.strip(" ")
            attrs = (f' fill="{fg}"' if fg else "") + (' font-weight="700"' if bold else "") + (' opacity=".55"' if dim else "") \
                + (' font-style="italic"' if italic else "") + (' text-decoration="underline"' if under else "")
            text.append(f'<text x="{n(x + lead * CW)}" y="{n(CH * 0.72)}" textLength="{n(len(chars) * CW)}"{attrs}>{html.escape(chars)}</text>')
        i = j
    return "".join(back + drawn + text)


def render(src, out, once, at):
    with open(src) as f:
        rec = json.load(f)
    # Screens too close together are merged, and long waits shortened.
    shown, clock, last_t, cut = [], 0.0, 0.0, 0
    for frame in rec["frames"]:
        clock += min(frame["t"] - last_t, MAX_GAP)
        cut += frame["t"] - last_t > MAX_GAP
        last_t = frame["t"]
        if shown and clock - shown[-1][0] < MIN_STEP:
            shown[-1] = (shown[-1][0], frame)
        else:
            shown.append((clock, frame))
    total = shown[-1][0] + LAST

    pool, strip = {}, []
    w, h = rec["cols"] * CW + 2 * PAD, rec["rows"] * CH + 2 * PAD
    for k, (_, frame) in enumerate(shown):
        uses = []
        for y, line in enumerate(frame["screen"].split("\n")[:rec["rows"]]):
            row = cells(line)
            if row:
                uses.append(f'<use href="#r{pool.setdefault(row, len(pool))}" y="{y * CH}"/>')
        cx, cy, visible = frame["cursor"]
        if visible:
            uses.append(f'<rect x="{n(cx * CW)}" y="{cy * CH + 2}" width="2" height="{CH - 4}"/>')
        strip.append(f'<g transform="translate({n(k * w)})">{"".join(uses)}</g>')

    steps = "".join(f"{100 * t / total:.3f}%{{transform:translateX(-{n(k * w)}px)}}" for k, (t, _) in enumerate(shown))
    if at is not None:   # one moment, standing still
        k = max(i for i, (t, _) in enumerate(shown) if t <= at)
        motion = f".s{{transform:translateX(-{n(k * w)}px)}}"
    else:
        motion = (f"@keyframes k{{{steps}}}.s{{animation:k {total:.2f}s step-end {'1 forwards' if once else 'infinite'}}}"
                  f"@media (prefers-reduced-motion:reduce){{.s{{animation:none;transform:translateX(-{n((len(shown) - 1) * w)}px)}}}}")
    label = f"A claude-jr session in a terminal: asked to fix failing tests, it runs them, edits the code and runs them again; then /stats shows its memory and CPU use."
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n(w)} {n(h)}" role="img" aria-label="{label}">'
           f"<title>{label}</title>"
           f"<style>text{{font:{SIZE}px ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,'Liberation Mono','DejaVu Sans Mono',monospace;"
           f"white-space:pre}}path{{stroke-width:1.1}}{motion}</style>"
           f'<rect width="{n(w)}" height="{n(h)}" rx="10" fill="{BG}"/>'
           "<defs>" + "".join(f'<g id="r{i}">{row_svg(row)}</g>' for row, i in pool.items()) + "</defs>"
           f'<svg x="{PAD}" y="{PAD}" width="{n(rec["cols"] * CW)}" height="{rec["rows"] * CH}" overflow="hidden">'
           f'<g class="s" fill="{FG}">{"".join(strip)}</g></svg></svg>\n')
    with open(out, "w") as f:
        f.write(svg)
    print(f"{out}: {len(svg) // 1024} KB, {len(shown)} screens, {len(pool)} different lines; the session took "
          f"{rec['seconds']:.1f}s and " + (f"{cut} waits were shortened" if cut else "plays in real time"))


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if args[:1] == ["record"] and len(args) == 2:
        record(args[1], "--mock" in sys.argv)
    elif args[:1] == ["render"] and len(args) in (3, 4):   # the fourth is --at's number
        render(args[1], args[2], "--once" in sys.argv, float(args[3]) if len(args) == 4 else None)
    else:
        sys.exit("usage: demo.py record OUT.json [--mock] | render IN.json OUT.svg [--once] [--at SECONDS]")
