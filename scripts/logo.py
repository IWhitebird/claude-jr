# Builds assets/logo.svg from assets/mascot.txt.
#
#   python3 scripts/logo.py

import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
COLORS = {"O": "#D97757", "D": "#B9583A", "K": "#2B211C", "W": "#FFFFFF",
          "P": "#F2A48B", "B": "#6FA8DC", "Y": "#F2C14E"}
PX, PAD = 10, 2

art = [l.rstrip("\n") for l in open(os.path.join(ROOT, "assets/mascot.txt")) if not l.startswith("#") and l.strip()]
w, h = len(art[0]), len(art)
rects = []
for y, row in enumerate(art):
    x = 0
    while x < w:
        c = row[x]
        run = 1
        while x + run < w and row[x + run] == c:
            run += 1
        if c != ".":
            rects.append(f'  <rect x="{(x + PAD) * PX}" y="{(y + PAD) * PX}" width="{run * PX}" '
                         f'height="{PX}" fill="{COLORS[c]}"/>')
        x += run
W, H = (w + 2 * PAD) * PX, (h + 2 * PAD) * PX
svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" '
       f'shape-rendering="crispEdges" role="img" aria-label="claude-jr">\n  <title>claude-jr</title>\n'
       + "\n".join(rects) + "\n</svg>\n")
open(os.path.join(ROOT, "assets/logo.svg"), "w").write(svg)
print(f"wrote assets/logo.svg ({len(rects)} rects)")
