#!/usr/bin/python3
# Turns the Markdown documents into pages of the website: docs/GUIDE.md becomes
# site/guide.html (served as /guide), and so on. The site is for people who
# use claude-jr, not for people who work on it: anything between
# <!-- repo only --> and <!-- /repo only --> is left out of the page (GitHub
# shows it; the comments themselves are invisible there). Run it after
# changing a document:
#
#   python3 docs/site.py
#
# Needs markdown-it-py (Debian/Ubuntu: python3-markdown-it).

import html, os, re
from markdown_it import MarkdownIt

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO = "https://github.com/IWhitebird/claude-jr"
PAGES = {  # file: (address, name in the menu, one line for search engines)
    "GUIDE.md": ("guide", "Guide", "How to use claude-jr: commands, keys, permission modes, tools, skills, MCP, the sandbox, other models, background tasks and config."),
    "BENCHMARKS.md": ("benchmarks", "Benchmarks", "Every number claude-jr publishes and how it was measured: size, memory, CPU, SWE-bench Verified, and the comparisons with other agents."),
    "ARCHITECTURE.md": ("architecture", "How it works", "How claude-jr works: its threads, what happens in one turn, and why the conversation is kept on disk."),
}
# Written but not announced yet: the page is made and can be opened at its
# address, but it is not in the menu and search engines are asked to skip it.
DRAFTS = {
    "SMALL.md": ("small", "", "How a coding agent fits in a 2 MB file and 0.7 MB of memory: what the binary is made of, and the handful of decisions that keep it there."),
}


def slug(text):
    """A heading's anchor, made the way GitHub makes them, so links work in both places."""
    text = re.sub(r"<[^>]+>", "", text).strip().lower()
    return re.sub(r"\s", "-", re.sub(r"[^\w\s-]", "", text))


def link(target):
    """Where a link in a document leads on the site."""
    if re.match(r"[a-z]+:|#", target):
        return target
    path, _, anchor = target.partition("#")
    anchor = "#" + anchor if anchor else ""
    name = os.path.basename(path)
    if name in PAGES or name in DRAFTS:
        return "/" + (PAGES.get(name) or DRAFTS[name])[0] + anchor
    if name == "README.md":
        return "/"
    return f"{REPO}/blob/main/{os.path.normpath(os.path.join('docs', path))}{anchor}"


def page(source):
    address, name, about = PAGES.get(source) or DRAFTS[source]
    text = open(os.path.join(ROOT, "docs", source)).read()
    # What only someone working on the code needs stays on GitHub.
    text = re.sub(r"<!-- repo only -->.*?<!-- /repo only -->\n?", "", text, flags=re.S)
    body = MarkdownIt("commonmark").enable("table").render(text)
    body = re.sub(r'href="([^"]+)"', lambda m: f'href="{html.escape(link(html.unescape(m.group(1))))}"', body)
    contents = []

    def heading(m):
        level, inner = m.group(1), m.group(2)
        anchor = slug(html.unescape(inner))
        if level == "2":
            contents.append(f'<a href="#{anchor}">{re.sub(r"<[^>]+>", "", inner)}</a>')
        return f'<h{level} id="{anchor}">{inner}</h{level}>'

    body = re.sub(r"<h([1-3])>(.*?)</h\1>", heading, body, flags=re.S)
    body = body.replace("<table>", '<div class="scroll"><table>').replace("</table>", "</table></div>")
    # Diagrams are written in Mermaid, which GitHub draws by itself; here the page loads the library for them.
    body, diagrams = re.subn(r'<pre><code class="language-mermaid">(.*?)</code></pre>', r'<pre class="mermaid">\1</pre>', body, flags=re.S)
    title = re.search(r"<h1[^>]*>(.*?)</h1>", body, re.S).group(1)
    menu = "".join(f'<a href="/{a}"{" aria-current=page" if a == address else ""}>{n}</a>' for a, n, _ in PAGES.values())
    out = TEMPLATE
    for key, value in (("TITLE", re.sub(r"<[^>]+>", "", title)), ("ABOUT", html.escape(about, quote=True)), ("ADDRESS", address),
                       ("MENU", menu), ("CONTENTS", "".join(contents)), ("SOURCE", f"{REPO}/blob/main/docs/{source}"), ("BODY", body),
                       ("ROBOTS", '<meta name="robots" content="noindex">\n' if source in DRAFTS else ""),
                       ("DIAGRAMS", MERMAID if diagrams else "")):
        out = out.replace("{{" + key + "}}", value)
    with open(os.path.join(ROOT, "site", address + ".html"), "w") as f:
        f.write(out)
    print(f"site/{address}.html: {len(out) // 1024} KB, {len(contents)} sections")


TEMPLATE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>{{TITLE}} · claude-jr</title>
<meta name="description" content="{{ABOUT}}">
<link rel="canonical" href="https://claudejr.iwhitebird.com/{{ADDRESS}}">
{{ROBOTS}}<link rel="icon" type="image/svg+xml" href="/logo.svg">
<meta name="theme-color" content="#f4f1e9" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#161412" media="(prefers-color-scheme: dark)">
<meta property="og:title" content="{{TITLE}} · claude-jr">
<meta property="og:description" content="{{ABOUT}}">
<meta property="og:image" content="https://claudejr.iwhitebird.com/og.png">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=Source+Serif+4:opsz,wght@8..60,400;8..60,500&family=JetBrains+Mono:wght@400;500&display=swap">
<style>
  /* The documents, set like the front page: paper, serif headings, rules. */
  :root {
    color-scheme: light dark;
    --paper: #f4f1e9; --ink: #1b1915; --ink-2: #4d4842; --ink-3: #736d63;
    --rule: #ddd7cb; --clay: #bf5a37; --clay-text: #a8492a; --code: #ebe6da;
    --serif: "Source Serif 4", Georgia, "Times New Roman", serif;
    --sans: "Inter", system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    --mono: "JetBrains Mono", ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  }
  @media (prefers-color-scheme: dark) {
    :root { --paper: #161412; --ink: #ece6dc; --ink-2: #b9b1a6; --ink-3: #968e83; --rule: #322d28; --clay: #e0805c; --clay-text: #e8906e; --code: #221f1b; }
  }
  * { box-sizing: border-box; }
  html { scroll-behavior: smooth; scroll-padding-top: 20px; }
  body { margin: 0; background: var(--paper); color: var(--ink); font: 16.5px/1.65 var(--sans); -webkit-font-smoothing: antialiased; }
  a { color: inherit; text-decoration-thickness: 1px; text-underline-offset: 3px; }
  a:hover { color: var(--clay-text); }
  :focus-visible { outline: 2px solid var(--clay); outline-offset: 2px; }
  .frame { max-width: 1040px; margin: 0 auto; padding-inline: max(22px, env(safe-area-inset-left)) max(22px, env(safe-area-inset-right)); }
  header.top { padding-block: 22px; border-bottom: 1px solid var(--rule); }
  header.top .frame { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
  header.top img { width: 30px; height: 26px; image-rendering: pixelated; }
  header.top .name { font: 500 17px var(--sans); text-decoration: none; }
  header.top nav { margin-left: auto; display: flex; gap: 22px; font-size: 15px; color: var(--ink-2); }
  @media (max-width: 560px) {
    header.top .frame { row-gap: 10px; }
    header.top nav { margin-left: 0; flex-basis: 100%; gap: 0; justify-content: space-between; }
  }
  header.top nav a { text-decoration: none; }
  header.top nav a[aria-current] { color: var(--ink); font-weight: 600; }
  .layout { display: grid; grid-template-columns: 200px minmax(0, 1fr); gap: 56px; padding-block: 40px 80px; }
  aside nav { position: sticky; top: 24px; display: flex; flex-direction: column; gap: 7px; font-size: 14.5px; max-height: calc(100vh - 48px); overflow-y: auto; }
  aside nav a { text-decoration: none; color: var(--ink-2); }
  aside b { font-size: 13px; font-weight: 500; color: var(--ink-3); margin-bottom: 4px; }
  @media (max-width: 820px) { .layout { grid-template-columns: minmax(0, 1fr); gap: 0; padding-top: 24px; } aside { display: none; } }
  article { min-width: 0; }
  article h1 { font: 400 clamp(2.1rem, 5vw, 2.9rem)/1.1 var(--serif); letter-spacing: -0.02em; margin: 0 0 20px; }
  article h2 { font: 400 1.75rem/1.2 var(--serif); letter-spacing: -0.01em; margin: 56px 0 12px; padding-top: 22px; border-top: 1px solid var(--rule); }
  article h3 { font: 600 1.05rem var(--sans); margin: 32px 0 8px; }
  article p, article li { color: var(--ink-2); }
  article strong { color: var(--ink); font-weight: 600; }
  article ul, article ol { padding-left: 1.3em; }
  article li { margin: 4px 0; }
  code { font: 0.86em var(--mono); background: var(--code); padding: 1px 5px; border-radius: 4px; }
  pre { background: var(--code); border-radius: 8px; padding: 14px 16px; overflow-x: auto; font: 13.5px/1.6 var(--mono); }
  pre code { background: none; padding: 0; font: inherit; }
  pre.mermaid { background: none; padding: 6px 0; margin: 22px 0; text-align: center; font: 12.5px/1.5 var(--mono); color: var(--ink-3); }
  pre.mermaid svg { max-width: 100%; height: auto; }
  .scroll { overflow-x: auto; margin: 18px 0; }
  table { border-collapse: collapse; width: 100%; font-size: 15px; font-variant-numeric: tabular-nums; }
  th, td { padding: 9px 16px 9px 0; border-bottom: 1px solid var(--rule); text-align: left; vertical-align: top; }
  th { font-weight: 500; color: var(--ink-3); font-size: 14px; border-bottom-color: var(--ink); white-space: nowrap; }
  td { color: var(--ink-2); }
  td:first-child { color: var(--ink); }
  /* On a phone a table must fit the screen: smaller type, headings that wrap, long names that break. */
  @media (max-width: 560px) {
    table { font-size: 14px; }
    th, td { padding-right: 10px; }
    th { white-space: normal; }
    td code, th code { overflow-wrap: anywhere; }
  }
  footer { border-top: 1px solid var(--rule); padding-block: 28px 48px; font-size: 14.5px; color: var(--ink-3); }
  footer .frame { display: flex; flex-wrap: wrap; gap: 8px 22px; }
  footer a { color: var(--ink-2); text-decoration: none; }
  footer .last { flex-basis: 100%; }
</style>
<script>window.va = window.va || function () { (window.vaq = window.vaq || []).push(arguments); };</script>
<script defer src="/_vercel/insights/script.js"></script>
</head>
<body>
<header class="top">
  <div class="frame">
    <img src="/logo.svg" alt="" width="30" height="26">
    <a class="name" href="/">claude-jr</a>
    <nav>{{MENU}}<a href="https://github.com/IWhitebird/claude-jr">GitHub</a></nav>
  </div>
</header>
<div class="frame layout">
  <aside><nav aria-label="On this page"><b>On this page</b>{{CONTENTS}}</nav></aside>
  <article>
{{BODY}}
  </article>
</div>
<footer>
  <div class="frame">
    <a href="/">Home</a>
    <a href="/guide">Guide</a>
    <a href="/benchmarks">Benchmarks</a>
    <a href="/architecture">How it works</a>
    <a href="https://github.com/IWhitebird/claude-jr">GitHub</a>
    <a href="https://github.com/IWhitebird/claude-jr/releases/latest">Releases</a>
    <a href="{{SOURCE}}">Edit this page</a>
    <span class="last">claude-jr is MIT licensed and independent; it isn't made by or affiliated with Anthropic.</span>
  </div>
</footer>
{{DIAGRAMS}}</body>
</html>
"""

# Draws the diagrams in the page's own colours, light or dark.
MERMAID = """<script type="module">
  import mermaid from "https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.esm.min.mjs";
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  const ink = dark ? "#ece6dc" : "#1b1915", soft = dark ? "#968e83" : "#736d63";
  const paper = dark ? "#161412" : "#f4f1e9", card = dark ? "#1d1a17" : "#f9f7f1", rule = dark ? "#3a342e" : "#cfc8ba";
  mermaid.initialize({
    startOnLoad: false,
    theme: "base",
    fontFamily: "Inter, system-ui, sans-serif",
    flowchart: { curve: "basis", htmlLabels: true, padding: 14 },
    themeVariables: {
      fontSize: "14px", background: paper, primaryColor: card, primaryTextColor: ink, primaryBorderColor: rule,
      lineColor: soft, secondaryColor: paper, tertiaryColor: paper, clusterBkg: paper, clusterBorder: rule,
      edgeLabelBackground: paper, titleColor: soft,
    },
  });
  // One at a time: drawn together, their parts can land in each other's frames.
  // A diagram drawn left to right can be too wide for its place and shrink
  // until it can't be read; then the same diagram drawn top to bottom is tried,
  // and whichever comes out larger is kept.
  const width = (svg) => +(svg.match(/viewBox="[-\\d.]+ [-\\d.]+ ([\\d.]+)/) || [0, 1])[1];
  let n = 0;
  for (const block of document.querySelectorAll("pre.mermaid")) {
    const text = block.textContent, room = block.clientWidth;
    let { svg } = await mermaid.render(`diagram-${n++}`, text);
    if (width(svg) > room * 1.25 && /^\\s*flowchart LR/.test(text)) {
      const other = (await mermaid.render(`diagram-${n++}`, text.replace("flowchart LR", "flowchart TB"))).svg;
      if (width(other) < width(svg)) svg = other;
    }
    block.innerHTML = svg;
  }
</script>
"""

if __name__ == "__main__":
    for source in [*PAGES, *DRAFTS]:
        if os.path.exists(os.path.join(ROOT, "docs", source)):
            page(source)
