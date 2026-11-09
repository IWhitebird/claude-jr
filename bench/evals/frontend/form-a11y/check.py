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

t = parse("signup.html")
inputs = [n for n in t.tags if n["tag"] == "input" and n["attrs"].get("type") != "hidden"]
need(len(inputs) == 4, f"expected 4 visible inputs, found {len(inputs)}")
labels = [n for n in t.tags if n["tag"] == "label"]
for i in inputs:
    name = i["attrs"].get("name")
    iid = i["attrs"].get("id")
    by_for = iid and any(l["attrs"].get("for") == iid for l in labels)
    # A label wrapping the input also counts; the parser can't see nesting of
    # void elements, so accept a label whose text names the field.
    wrapped = any(name and name.lower()[:4] in l["text"].lower() for l in labels)
    need(by_for or wrapped, f"input {name} has no label")
need(all(l["text"].strip() for l in labels), "a label has no visible text")
buttons = [n for n in t.tags if n["tag"] == "button"]
need(any(b["attrs"].get("type") == "submit" and "Sign up" in b["text"] for b in buttons), "the Sign up button needs type=submit")
close = next((b for b in buttons if "close" in (b["attrs"].get("class") or "")), None)
need(close is not None, "the close button is gone")
named = close["attrs"].get("aria-label") or close["attrs"].get("title") or any(
    n for n in t.tags if n["tag"] == "span" and "sr-only" in (n["attrs"].get("class") or "") and n["text"].strip())
need(bool(named), "the close button needs an accessible name (aria-label)")
print("pass")
