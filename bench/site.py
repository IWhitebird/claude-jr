#!/usr/bin/env python3
# Checks the website's pages at several screen widths in a real browser
# (headless Chrome): nothing may be wider than the screen or need sideways
# scrolling that isn't a table or a code block, the top bar's links must sit
# in one row, a diagram must not be shrunk until it can't be read, every image
# must load and the front page's script must run to its end. It also
# saves a screenshot of every page at every width (and two in the dark
# colours), to be looked at.
#
#   python3 bench/site.py                 # every page; screenshots in bench/results/site/
#   python3 bench/site.py architecture    # one page
#
# Needs google-chrome (or chromium) on PATH and, for the diagrams, the network.

import http.server, json, os, re, shutil, subprocess, sys, tempfile, threading

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WIDTHS = [360, 390, 768, 1024, 1280, 1920]
PAGES = ["index", "guide", "benchmarks", "architecture", "small"]
CHROME = shutil.which("google-chrome") or shutil.which("chromium") or shutil.which("chromium-browser")

# Loads a page in a frame of the width asked for and writes down what sticks out.
CHECKER = """<!doctype html><meta charset="utf-8"><body style="margin:0">
<iframe id="f" style="border:0;display:block"></iframe><pre id="report">waiting</pre>
<script>
const q = new URLSearchParams(location.search), width = +q.get("w"), frame = document.getElementById("f");
frame.style.width = width + "px"; frame.style.height = "900px"; frame.src = q.get("page") + ".html";
frame.onload = () => setTimeout(() => {
  const doc = frame.contentDocument, view = doc.documentElement.clientWidth, out = [];
  const name = (e) => e.tagName.toLowerCase() + (e.id ? "#" + e.id : "") + (e.className && e.className.baseVal === undefined ? "." + String(e.className).trim().split(/\\s+/).join(".") : "");
  const scrolls = (e) => { for (let p = e.parentElement; p; p = p.parentElement) { const o = getComputedStyle(p).overflowX; if (o === "auto" || o === "scroll" || o === "hidden") return p; } return null; };
  if (doc.documentElement.scrollWidth > view + 1) out.push({ what: "the page scrolls sideways", by: doc.documentElement.scrollWidth - view });
  for (const e of doc.body.querySelectorAll("*")) {
    const r = e.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    if (r.right > view + 1 && !scrolls(e)) out.push({ what: "wider than the screen", el: name(e), by: Math.round(r.right - view) });
    const o = getComputedStyle(e).overflowX;
    if ((o === "auto" || o === "scroll") && e.scrollWidth > e.clientWidth + 1 && !e.matches(".scroll, pre:not(.mermaid), .term"))
      out.push({ what: "has to be scrolled sideways", el: name(e), by: e.scrollWidth - e.clientWidth });
    if (e.matches("pre.mermaid") && !e.querySelector("svg")) out.push({ what: "a diagram was not drawn", el: name(e) });
    if (e.tagName === "svg" && e.closest("pre.mermaid") && r.width > e.closest("pre.mermaid").clientWidth + 1) out.push({ what: "a diagram is wider than its place", by: Math.round(r.width - e.closest("pre.mermaid").clientWidth) });
  }
  // The top bar: its links in one row, not running into the name.
  const links = [...doc.querySelectorAll("header.top nav a")], tops = new Set(links.map((a) => Math.round(a.getBoundingClientRect().top / 4)));
  if (tops.size > 1) out.push({ what: "the top bar's links wrap onto more than one row" });
  if (links.length !== 4) out.push({ what: "the top bar should have 4 links, has " + links.length });
  if (!doc.querySelector("footer a[href='/guide']")) out.push({ what: "the footer has no links" });
  // On a phone a table should fit: columns cut off at the edge are easy to miss.
  if (width <= 400) for (const box of doc.querySelectorAll(".scroll")) {
    if (box.scrollWidth > box.clientWidth + 1) out.push({ what: "a table has to be scrolled sideways on a phone: " + (box.querySelector("th, td") || box).textContent.trim().slice(0, 30), by: box.scrollWidth - box.clientWidth });
  }
  // Text drawn in a picture must be large enough to read.
  const tiny = [...doc.querySelectorAll("svg text")].filter((t) => { const r = t.getBoundingClientRect(); return r.height > 0 && r.height < 11; });
  if (tiny.length) out.push({ what: "text in a drawing is too small to read (" + Math.round(tiny[0].getBoundingClientRect().height) + "px high): " + tiny[0].textContent.slice(0, 30) });
  // The front page's script marks its own end: one mistake anywhere in it and nothing on the page works.
  if (q.get("page") === "index" && !doc.documentElement.dataset.ran) out.push({ what: "the page's script stopped on an error" });
  // An image that didn't load shows as a gap.
  for (const img of doc.images) if (!img.complete || !img.naturalWidth) out.push({ what: "an image did not load: " + img.getAttribute("src") });
  // A diagram shrunk to fit must still be readable.
  for (const svg of doc.querySelectorAll("pre.mermaid svg")) {
    const box = svg.viewBox.baseVal, scale = box && box.width ? svg.getBoundingClientRect().width / box.width : 1;
    if (scale < 0.7) out.push({ what: "a diagram is drawn at " + Math.round(scale * 100) + "% of its size, too small to read" });
  }
  document.getElementById("report").textContent = "REPORT" + JSON.stringify({ height: doc.documentElement.scrollHeight, problems: out.slice(0, 12) }) + "END";
}, +q.get("wait") || 4000);
</script>"""


def chrome(*args):
    return subprocess.run([CHROME, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars", *args],
                          capture_output=True, text=True, timeout=120).stdout


def main():
    if not CHROME:
        sys.exit("google-chrome or chromium is needed")
    pages = sys.argv[1:] or PAGES
    work = tempfile.mkdtemp(prefix="cjr-site-")
    shutil.copytree(os.path.join(ROOT, "site"), work, dirs_exist_ok=True)
    open(os.path.join(work, "_check.html"), "w").write(CHECKER)
    handler = lambda *a, **k: http.server.SimpleHTTPRequestHandler(*a, directory=work, **k)
    http.server.SimpleHTTPRequestHandler.log_message = lambda *a: None
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_address[1]}"
    shots = os.path.join(ROOT, "bench/results/site")
    os.makedirs(shots, exist_ok=True)
    problems = 0
    for page in pages:
        wait = 9000 if "mermaid" in open(os.path.join(work, page + ".html")).read() else 2500
        for width in WIDTHS:
            dom = chrome(f"--window-size={max(width, 500)},1000", f"--virtual-time-budget={wait + 4000}", "--dump-dom",
                         f"{base}/_check.html?page={page}&w={width}&wait={wait}")
            found = re.search(r"REPORT(.*?)END", dom, re.S)
            read = json.loads(found.group(1).replace("&quot;", '"').replace("&amp;", "&")) if found else {}
            report = read.get("problems", [{"what": "the check did not run"}])
            # The whole page, top to bottom, at this width.
            chrome(f"--window-size={width},{min(read.get('height', 4000) + 40, 16000)}", f"--virtual-time-budget={wait + 4000}",
                   f"--screenshot={shots}/{page}-{width}.png", f"{base}/{page}.html")
            if width in (390, 1280):  # and in the dark colours
                chrome(f"--window-size={width},{min(read.get('height', 4000) + 40, 16000)}", f"--virtual-time-budget={wait + 4000}",
                       "--force-dark-mode", f"--screenshot={shots}/{page}-{width}-dark.png", f"{base}/{page}.html")
            problems += len(report)
            print(f"{page:<13} {width:>5}px  {'ok' if not report else ''}")
            for r in report:
                print(f"    {r['what']}" + (f": {r['el']}" if r.get("el") else "") + (f" (by {r['by']}px)" if r.get("by") else ""))
    server.shutdown()
    shutil.rmtree(work, ignore_errors=True)
    print(f"\n{problems} problem{'s' if problems != 1 else ''}; screenshots in {os.path.relpath(shots, ROOT)}/")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
