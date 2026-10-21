"""Copies a Catch test file without the named TEST_CASEs.

Usage: python3 -I strip_cases.py <source.cpp> <target.cpp> [test case name...]

Used for the C API test cases that reach into DuckDB's C++ API (they cast C handles to
duckdb::Vector or duckdb::Connection), which cannot compile against duckdb.h alone.
"""

import sys


def case_end(text: str, start: int) -> int:
    """The index just past the body of the TEST_CASE starting at `start`."""
    i = text.index("{", text.index(")", start))
    depth = 0
    while i < len(text):
        c = text[i]
        if text.startswith("//", i):
            i = text.index("\n", i)
            continue
        if text.startswith("/*", i):
            i = text.index("*/", i) + 2
            continue
        if c in "\"'":
            i += 1
            while text[i] != c:
                i += 2 if text[i] == "\\" else 1
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    raise ValueError("unbalanced TEST_CASE body")


def main() -> None:
    source, target, *names = sys.argv[1:]
    text = open(source, encoding="utf-8").read()
    for name in names:
        marker = f'TEST_CASE("{name}"'
        start = text.find(marker)
        if start < 0:
            continue
        text = text[:start] + f"// (removed: {name})" + text[case_end(text, start):]
    open(target, "w", encoding="utf-8").write(text)


main()
