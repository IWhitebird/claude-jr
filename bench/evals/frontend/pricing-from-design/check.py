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

t = parse("pricing.html")
cards = [n for n in t.tags if "card" in (n["attrs"].get("class") or "").split()]
need(len(cards) == 3, f"expected 3 elements with class card, found {len(cards)}")
plans = {"Starter": "$9", "Pro": "$29", "Team": "$99"}
for plan, price in plans.items():
    card = next((c for c in cards if plan in c["text"]), None)
    need(card is not None, f"no card for {plan}")
    need(price in card["text"].replace(" ", ""), f"{plan} should cost {price}")
pro = next(c for c in cards if "Pro" in c["text"])
need("Most popular" in pro["text"], "the Pro card should carry the Most popular badge")
need("<style" in open("pricing.html").read(), "the CSS should be in the file")
print("pass")
