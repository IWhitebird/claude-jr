# Exit 0 if the task is done, 1 with a reason if not. Runs in the agent's working directory.
import re, sys
css = re.sub(r"/\*.*?\*/", "", open("style.css").read(), flags=re.S)

def need(ok, why):
    if not ok:
        print("FAIL:", why); sys.exit(1)

def block(selector, text):
    m = re.search(re.escape(selector) + r"\s*\{([^}]*)\}", text)
    return m.group(1) if m else None

root = block(":root", css)
need(root and len(re.findall(r"--[\w-]+\s*:", root)) >= 3, ":root should define at least 3 color variables")
dark = re.search(r"@media[^{]*prefers-color-scheme\s*:\s*dark[^{]*\{(.*?\})\s*\}", css, re.S)
need(dark, "no prefers-color-scheme: dark media query")
dark_root = block(":root", dark.group(1))
need(dark_root and len(re.findall(r"--[\w-]+\s*:", dark_root)) >= 2, "the dark query should override the variables on :root")
outside = css[:dark.start()] + css[dark.end():]
for sel in ("body", ".card", "a"):
    b = block(sel, re.sub(r":root\s*\{[^}]*\}", "", outside))
    need(b, f"no {sel} rule")
    need("var(--" in b, f"{sel} should use the variables")
    need(not re.search(r"(?<!-)(background|color|border)[^:;]*:\s*[^;]*#[0-9a-fA-F]{3,6}", b), f"{sel} still has a fixed color")
print("pass")
