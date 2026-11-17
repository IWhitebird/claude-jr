# The work for bench/workers/run.py: a small Python package with ten modules,
# each with one bug and a test file that fails because of it. The pieces don't
# depend on each other, which is the case handing work out is meant for.
#
#   python3 bench/workers/fixture.py        checks the fixture itself: every test
#                                           fails as shipped and passes once fixed

import os, subprocess, sys

# name: (module source, the buggy text in it, what it should be, test source)
MODULES = {
    "slug": ('''import re


def slugify(text):
    """Lower-case words joined by single dashes: "Hello,  World!" -> "hello-world"."""
    text = text.strip().lower()
    text = re.sub(r"[^a-z0-9]", "-", text)
    return text
''', '''    text = re.sub(r"[^a-z0-9]", "-", text)
    return text
''', '''    text = re.sub(r"[^a-z0-9]+", "-", text)
    return text.strip("-")
''', '''
    def test_words(self):
        self.assertEqual(slugify("Hello World"), "hello-world")

    def test_runs_of_punctuation_become_one_dash(self):
        self.assertEqual(slugify("Hello,  World!"), "hello-world")

    def test_no_dashes_at_the_ends(self):
        self.assertEqual(slugify("  (draft) Plan 9 "), "draft-plan-9")
'''),
    "duration": ('''import re

UNITS = {"h": 3600, "m": 60, "s": 1}


def parse_duration(text):
    """Seconds in a duration like "1h30m", "45s" or "2h5s". Raises ValueError otherwise."""
    parts = re.findall(r"(\\d+)([hms])", text)
    if not parts:
        raise ValueError(text)
    return sum(int(n) * UNITS[u] for n, u in parts)
''', '''    parts = re.findall(r"(\\d+)([hms])", text)
    if not parts:
        raise ValueError(text)
''', '''    if not re.fullmatch(r"(\\d+[hms])+", text):
        raise ValueError(text)
    parts = re.findall(r"(\\d+)([hms])", text)
''', '''
    def test_units(self):
        self.assertEqual(parse_duration("1h30m"), 5400)
        self.assertEqual(parse_duration("45s"), 45)
        self.assertEqual(parse_duration("2h5s"), 7205)

    def test_nonsense_is_refused(self):
        for bad in ["", "soon", "1h30", "h", "1h 30m", "10x5m"]:
            with self.assertRaises(ValueError, msg=bad):
                parse_duration(bad)
'''),
    "lru": ('''class LRU:
    """A cache of at most `size` items; the one used longest ago goes first."""

    def __init__(self, size):
        self.size = size
        self.items = {}

    def get(self, key, default=None):
        return self.items.get(key, default)

    def put(self, key, value):
        self.items.pop(key, None)
        self.items[key] = value
        if len(self.items) > self.size:
            del self.items[next(iter(self.items))]
''', '''    def get(self, key, default=None):
        return self.items.get(key, default)
''', '''    def get(self, key, default=None):
        if key not in self.items:
            return default
        self.items[key] = self.items.pop(key)
        return self.items[key]
''', '''
    def test_holds_up_to_its_size(self):
        c = LRU(2)
        c.put("a", 1)
        c.put("b", 2)
        c.put("c", 3)
        self.assertIsNone(c.get("a"))
        self.assertEqual((c.get("b"), c.get("c")), (2, 3))

    def test_reading_counts_as_use(self):
        c = LRU(2)
        c.put("a", 1)
        c.put("b", 2)
        self.assertEqual(c.get("a"), 1)
        c.put("c", 3)
        self.assertEqual(c.get("a"), 1)
        self.assertIsNone(c.get("b"))

    def test_a_missing_key_gives_the_default(self):
        self.assertEqual(LRU(1).get("x", 7), 7)
'''),
    "roman": ('''NUMERALS = [(1000, "M"), (500, "D"), (100, "C"), (50, "L"), (10, "X"), (5, "V"), (1, "I")]


def to_roman(n):
    """A whole number from 1 to 3999 as a Roman numeral: 1994 -> "MCMXCIV"."""
    if not 0 < n < 4000:
        raise ValueError(n)
    out = ""
    for value, letter in NUMERALS:
        while n >= value:
            out += letter
            n -= value
    return out
''', '''NUMERALS = [(1000, "M"), (500, "D"), (100, "C"), (50, "L"), (10, "X"), (5, "V"), (1, "I")]
''', '''NUMERALS = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"),
            (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")]
''', '''
    def test_plain(self):
        self.assertEqual(to_roman(3), "III")
        self.assertEqual(to_roman(2026), "MMXXVI")

    def test_subtractive_forms(self):
        self.assertEqual(to_roman(4), "IV")
        self.assertEqual(to_roman(9), "IX")
        self.assertEqual(to_roman(1994), "MCMXCIV")
        self.assertEqual(to_roman(444), "CDXLIV")

    def test_range(self):
        for bad in [0, -1, 4000]:
            with self.assertRaises(ValueError):
                to_roman(bad)
'''),
    "fields": ('''def split_row(line):
    """The fields of one CSV line. A field in double quotes may hold commas,
    and "" inside it is one quote: 'a,"b,c",d' -> ["a", "b,c", "d"]."""
    return [field.strip('"') for field in line.split(",")]
''', '''    return [field.strip('"') for field in line.split(",")]
''', '''    fields, field, quoted, i = [], "", False, 0
    while i < len(line):
        c = line[i]
        if quoted and c == '"' and line[i + 1:i + 2] == '"':
            field += '"'
            i += 1
        elif c == '"':
            quoted = not quoted
        elif c == "," and not quoted:
            fields.append(field)
            field = ""
        else:
            field += c
        i += 1
    return fields + [field]
''', '''
    def test_plain(self):
        self.assertEqual(split_row("a,b,c"), ["a", "b", "c"])
        self.assertEqual(split_row("a,,c"), ["a", "", "c"])

    def test_quoted_commas(self):
        self.assertEqual(split_row('a,"b,c",d'), ["a", "b,c", "d"])

    def test_doubled_quotes(self):
        self.assertEqual(split_row('"say ""hi""",x'), ['say "hi"', "x"])
'''),
    "intervals": ('''def merge(intervals):
    """Overlapping or touching (start, end) pairs joined, in order:
    [(1, 3), (3, 5), (8, 9)] -> [(1, 5), (8, 9)]."""
    out = []
    for start, end in sorted(intervals):
        if out and start < out[-1][1]:
            out[-1] = (out[-1][0], end)
        else:
            out.append((start, end))
    return out
''', '''        if out and start < out[-1][1]:
            out[-1] = (out[-1][0], end)
''', '''        if out and start <= out[-1][1]:
            out[-1] = (out[-1][0], max(out[-1][1], end))
''', '''
    def test_overlapping(self):
        self.assertEqual(merge([(1, 4), (2, 6), (8, 9)]), [(1, 6), (8, 9)])

    def test_touching(self):
        self.assertEqual(merge([(1, 3), (3, 5)]), [(1, 5)])

    def test_one_inside_another(self):
        self.assertEqual(merge([(1, 10), (2, 3)]), [(1, 10)])

    def test_unsorted_and_empty(self):
        self.assertEqual(merge([(5, 6), (1, 2)]), [(1, 2), (5, 6)])
        self.assertEqual(merge([]), [])
'''),
    "wrap": ('''def wrap(text, width):
    """The words of `text` on lines of at most `width` characters. A word
    longer than that gets a line to itself, unbroken."""
    lines, line = [], ""
    for word in text.split():
        if len(line) + len(word) > width:
            lines.append(line)
            line = word
        else:
            line = (line + " " + word).strip()
    return lines + [line]
''', '''        if len(line) + len(word) > width:
            lines.append(line)
            line = word
        else:
            line = (line + " " + word).strip()
    return lines + [line]
''', '''        if line and len(line) + 1 + len(word) > width:
            lines.append(line)
            line = word
        else:
            line = (line + " " + word).strip()
    return lines + [line] if line else lines
''', '''
    def test_fits(self):
        self.assertEqual(wrap("one two", 10), ["one two"])

    def test_the_space_counts(self):
        self.assertEqual(wrap("abcd efgh", 8), ["abcd", "efgh"])
        self.assertEqual(wrap("abc defg", 8), ["abc defg"])

    def test_a_long_word_stands_alone(self):
        self.assertEqual(wrap("a extraordinarily b", 5), ["a", "extraordinarily", "b"])
        self.assertEqual(wrap("extraordinarily", 5), ["extraordinarily"])

    def test_nothing(self):
        self.assertEqual(wrap("", 5), [])
'''),
    "version": ('''def newer(a, b):
    """Whether version `a` is newer than `b`: "1.10.0" is newer than "1.9.3",
    and a missing part counts as 0, so "2" equals "2.0.0"."""
    return a.split(".") > b.split(".")
''', '''    return a.split(".") > b.split(".")
''', '''    left = [int(part) for part in a.split(".")]
    right = [int(part) for part in b.split(".")]
    size = max(len(left), len(right))
    return left + [0] * (size - len(left)) > right + [0] * (size - len(right))
''', '''
    def test_simple(self):
        self.assertTrue(newer("1.2.0", "1.1.9"))
        self.assertFalse(newer("1.1.9", "1.2.0"))

    def test_numbers_not_text(self):
        self.assertTrue(newer("1.10.0", "1.9.3"))
        self.assertFalse(newer("1.9.3", "1.10.0"))

    def test_missing_parts_are_zero(self):
        self.assertFalse(newer("2.0.0", "2"))
        self.assertFalse(newer("2", "2.0.0"))
        self.assertTrue(newer("2.0.1", "2"))
'''),
    "flatten": ('''def flatten(tree, sep="."):
    """A nested dict as one level, keys joined by `sep`:
    {"a": {"b": 1}, "c": 2} -> {"a.b": 1, "c": 2}. An empty dict stays as a value."""
    out = {}
    for key, value in tree.items():
        if isinstance(value, dict):
            for inner, leaf in flatten(value).items():
                out[key + sep + inner] = leaf
        else:
            out[key] = value
    return out
''', '''        if isinstance(value, dict):
            for inner, leaf in flatten(value).items():
''', '''        if isinstance(value, dict) and value:
            for inner, leaf in flatten(value, sep).items():
''', '''
    def test_nested(self):
        self.assertEqual(flatten({"a": {"b": 1}, "c": 2}), {"a.b": 1, "c": 2})

    def test_the_separator_goes_all_the_way_down(self):
        self.assertEqual(flatten({"a": {"b": {"c": 1}}}, sep="/"), {"a/b/c": 1})

    def test_an_empty_dict_is_a_value(self):
        self.assertEqual(flatten({"a": {}, "b": {"c": {}}}), {"a": {}, "b.c": {}})
'''),
    "average": ('''def moving_average(values, window):
    """The mean of each run of `window` values in a row:
    moving_average([1, 2, 3, 4], 2) -> [1.5, 2.5, 3.5]. Fewer values than the window: []."""
    out = []
    for i in range(len(values) - window):
        out.append(sum(values[i:i + window]) / window)
    return out
''', '''    for i in range(len(values) - window):
''', '''    for i in range(len(values) - window + 1):
''', '''
    def test_runs(self):
        self.assertEqual(moving_average([1, 2, 3, 4], 2), [1.5, 2.5, 3.5])

    def test_the_whole_list_is_one_run(self):
        self.assertEqual(moving_average([2, 4, 6], 3), [4.0])

    def test_too_few(self):
        self.assertEqual(moving_average([1], 2), [])
        self.assertEqual(moving_average([], 1), [])
'''),
}

EXPORTS = {"slug": "slugify", "duration": "parse_duration", "lru": "LRU", "roman": "to_roman", "fields": "split_row",
           "intervals": "merge", "wrap": "wrap", "version": "newer", "flatten": "flatten", "average": "moving_average"}


def package(root, names=None, fixed=False):
    """Writes the package into the folder `root`."""
    os.makedirs(os.path.join(root, "pkg"))
    os.makedirs(os.path.join(root, "tests"))
    for d in ("pkg", "tests"):
        open(os.path.join(root, d, "__init__.py"), "w").close()
    for name in names or MODULES:
        source, bug, fix, tests = MODULES[name]
        assert bug in source, name
        with open(os.path.join(root, "pkg", name + ".py"), "w") as f:
            f.write(source.replace(bug, fix) if fixed else source)
        with open(os.path.join(root, "tests", f"test_{name}.py"), "w") as f:
            f.write(f"import unittest\n\nfrom pkg.{name} import {EXPORTS[name]}\n\n\nclass Test(unittest.TestCase):{tests}")
    with open(os.path.join(root, "README.md"), "w") as f:
        f.write("# pkg\n\nSmall helpers. Each module in `pkg/` has its tests in `tests/`:\n\n"
                "    python3 -m unittest tests.test_NAME\n")


def commit(root):
    git = ["git", "-c", "user.name=bench", "-c", "user.email=bench@example.com", "-c", "commit.gpgsign=false"]
    for args in (["init", "-q", "-b", "main"], ["add", "-A"], ["commit", "-q", "-m", "the code as found"]):
        subprocess.run(git + args, cwd=root, check=True)


def write(root, names=None, fixed=False):
    """The package in `root`, as a new git repository with one commit."""
    package(root, names, fixed)
    commit(root)


def services(count, each):
    """`count` folders (svc01, svc02, ...) and the `each` modules in each: a
    different run of the ten for every folder, so no two hold the same set."""
    names = list(MODULES)
    return {f"svc{i + 1:02}": [names[(i * 3 + j) % len(names)] for j in range(each)] for i in range(count)}


def write_services(root, plan, fixed=False):
    """A repository of several packages, each in a folder of its own (see `services`)."""
    os.makedirs(root)
    for folder, names in plan.items():
        package(os.path.join(root, folder), names, fixed)
    commit(root)


def passing(root, names=None):
    """The modules whose tests pass in `root`."""
    ok = []
    for name in names or MODULES:
        run = subprocess.run([sys.executable, "-m", "unittest", "-q", f"tests.test_{name}"], cwd=root,
                             capture_output=True, timeout=60)
        if run.returncode == 0:
            ok.append(name)
    return ok


if __name__ == "__main__":
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        write(os.path.join(tmp, "found"))
        write(os.path.join(tmp, "fixed"), fixed=True)
        found, fixed = passing(os.path.join(tmp, "found")), passing(os.path.join(tmp, "fixed"))
        plan = services(10, 6)
        write_services(os.path.join(tmp, "many"), plan)
        write_services(os.path.join(tmp, "many-fixed"), plan, fixed=True)
        many = sum(len(passing(os.path.join(tmp, "many", f), n)) for f, n in plan.items())
        many_fixed = sum(len(passing(os.path.join(tmp, "many-fixed", f), n)) for f, n in plan.items())
    print(f"as shipped: {len(found)} of {len(MODULES)} pass {found}; once fixed: {len(fixed)} pass")
    print(f"ten services of six: {many} of 60 pass as shipped, {many_fixed} once fixed; "
          f"{len(set(map(tuple, plan.values())))} different sets")
    sys.exit(0 if not found and len(fixed) == len(MODULES) and many == 0 and many_fixed == 60 else 1)
