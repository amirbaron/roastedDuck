"""Copies a Catch test file without the named TEST_CASEs and SECTIONs.

Usage: python3 -I strip_cases.py <source.cpp> <target.cpp> [name...]

A name removes the TEST_CASE of that name; `SECTION:<name>` removes every SECTION of that
name instead. Used for the C API tests that reach into DuckDB's C++ API (they cast C
handles to duckdb::Vector or duckdb::Connection), which cannot compile against duckdb.h
alone.
"""

import sys


def block_end(text: str, start: int) -> int:
    """The index just past the braced body of the TEST_CASE or SECTION at `start`."""
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
    raise ValueError("unbalanced block")


def remove_blocks(text: str, marker: str, label: str) -> str:
    """Replaces every block starting with `marker` by a comment naming `label`."""
    while (start := text.find(marker)) >= 0:
        text = text[:start] + f"// (removed: {label})" + text[block_end(text, start):]
    return text


def main() -> None:
    source, target, *names = sys.argv[1:]
    text = open(source, encoding="utf-8").read()
    for name in names:
        if name.startswith("SECTION:"):
            section = name.removeprefix("SECTION:")
            text = remove_blocks(text, f'SECTION("{section}"', name)
        else:
            text = remove_blocks(text, f'TEST_CASE("{name}"', name)
    open(target, "w", encoding="utf-8").write(text)


main()
