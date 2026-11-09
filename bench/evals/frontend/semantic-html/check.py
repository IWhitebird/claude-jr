# Exit 0 if the task is done, 1 with a reason if not. Runs in the agent's working directory.
import sys
from html.parser import HTMLParser

class Tree(HTMLParser):
    """Collects every tag with its attributes and the text inside it."""
    def __init__(self):
        super().__init__()
        self.tags, self.stack, self.text = [], [], []
    def handle_starttag(self, tag, attrs):
        node = {"tag": tag, "attrs": dict(attrs), "text": ""}
        self.tags.append(node)
        if tag not in ("img", "input", "br", "meta", "link", "hr"):
            self.stack.append(node)
    def handle_endtag(self, tag):
        for i in range(len(self.stack) - 1, -1, -1):
            if self.stack[i]["tag"] == tag:
                del self.stack[i:]
                break
    def handle_data(self, data):
        self.text.append(data)
        for node in self.stack:
            node["text"] += data

def parse(path):
    t = Tree()
    t.feed(open(path).read())
    return t

def need(ok, why):
    if not ok:
        print("FAIL:", why)
        sys.exit(1)

t = parse("index.html")
tags = [n["tag"] for n in t.tags]
for tag in ("header", "nav", "main", "footer"):
    need(tag in tags, f"no <{tag}>")
nav = next(n for n in t.tags if n["tag"] == "nav")
for link in ("Home", "Posts", "About"):
    need(link in nav["text"], f"nav is missing {link}")
header = next(n for n in t.tags if n["tag"] == "header")
need("Home" in header["text"], "the nav isn't inside the header")
need("Shipping smaller bundles" in next(n for n in t.tags if n["tag"] == "main")["text"], "the post isn't in <main>")
need("2026 Acme" in next(n for n in t.tags if n["tag"] == "footer")["text"], "the footer text moved")
img = next((n for n in t.tags if n["tag"] == "img"), None)
need(img is not None and len(img["attrs"].get("alt") or "") >= 5, "the image needs a useful alt text")
for cls in ("logo", "title", "post"):
    need(any(cls in (n["attrs"].get("class") or "").split() for n in t.tags), f"class {cls} was lost")
print("pass")
