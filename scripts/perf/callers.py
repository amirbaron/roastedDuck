"""Reads a macOS `sample` call tree and prints, for the functions whose name contains
PATTERN, their self samples grouped by caller chain (the DEPTH frames above them).

usage: python3 -I scripts/perf/callers.py SAMPLE.txt PATTERN [DEPTH]
"""
import collections
import re
import sys

LINE = re.compile(r"^(?P<indent>[ +!:|]*)(?P<count>\d+) (?P<name>.+?)  \(in ")


def frames(path):
    """Yields (depth, count, name) for each call tree line."""
    in_tree = False
    for line in open(path, errors="replace"):
        if line.startswith("Call graph:"):
            in_tree = True
            continue
        if in_tree and line.startswith("Total number in stack"):
            return
        m = LINE.match(line)
        if in_tree and m:
            yield len(m.group("indent")), int(m.group("count")), m.group("name")


def short(name):
    """The name without generic arguments and with at most its last three path segments
    (symbols demangled by `rustfilt`, see scripts/perf/profile.sh)."""
    while True:
        stripped = re.sub(r"<[^<>]*>", "", name)
        if stripped == name:
            break
        name = stripped
    return "::".join(name.split("::")[-3:])[:100]


def main():
    path, pattern = sys.argv[1], sys.argv[2]
    depth = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    stack = []
    totals = collections.Counter()
    nodes = list(frames(path))
    for i, (level, count, name) in enumerate(nodes):
        while stack and stack[-1][0] >= level:
            stack.pop()
        stack.append((level, name))
        if pattern not in name:
            continue
        children = 0
        child_level = None
        for level2, count2, _ in nodes[i + 1:]:
            if level2 <= level:
                break
            child_level = child_level or level2
            if level2 == child_level:
                children += count2
        own = count - children
        if own > 0:
            chain = " <- ".join(short(n) for _, n in reversed(stack[-depth - 1:-1]))
            totals[chain] += own
    for chain, count in totals.most_common(25):
        print(f"{count:7d}  {chain}")


main()
