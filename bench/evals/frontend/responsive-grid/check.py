# Exit 0 if the task is done, 1 with a reason if not. Runs in the agent's working directory.
import re, sys
css = open("style.css").read()
css = re.sub(r"/\*.*?\*/", "", css, flags=re.S)

def need(ok, why):
    if not ok:
        print("FAIL:", why); sys.exit(1)

media = re.search(r"@media[^{]*min-width\s*:\s*(\d+)px[^{]*\{(.*?\})\s*\}", css, re.S)
need(media, "no min-width media query")
need(int(media.group(1)) == 640, f"breakpoint is {media.group(1)}px, not 640px")
inner = media.group(2)
need(re.search(r"\.cards\s*\{[^}]*grid-template-columns\s*:\s*(repeat\(\s*3\s*,\s*(1fr|minmax\(0,\s*1fr\))\s*\)|1fr\s+1fr\s+1fr)", inner),
     "inside the media query .cards should have three equal columns")
base = css[:media.start()] + css[media.end():]
rule = re.search(r"\.cards\s*\{([^}]*)\}", base)
need(rule, "no base .cards rule")
cols = re.search(r"grid-template-columns\s*:\s*([^;]+);", rule.group(1))
need(cols is None or cols.group(1).strip() in ("1fr", "minmax(0, 1fr)", "repeat(1, 1fr)", "100%"),
     "the base .cards rule should be one column")
need("display" in rule.group(1) and "grid" in rule.group(1), "the base rule lost display: grid")
print("pass")
