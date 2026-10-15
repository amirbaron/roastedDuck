#!/usr/bin/env python3
"""Extract a parser test corpus from DuckDB's sqllogictest files.

Collects the SQL of every `statement` and `query` block under `test/` (the lines
after the header, up to a blank line or `----`), skipping blocks that still contain
unsubstituted loop variables (`${...}`). Writes deduplicated records in the
length-prefixed format the oracle and `examples/corpus_check.rs` read:

    <byte length>\\n<sql bytes>

    python3 crates/pg-parser/scripts/extract_corpus.py [duckdb-root] [output]
"""

from __future__ import annotations

import pathlib
import re
import sys

import model as M

HEADER = re.compile(r"^(statement|query)\b")


def blocks(path: pathlib.Path):
    lines = path.read_text(errors="surrogateescape").splitlines()
    i = 0
    while i < len(lines):
        if HEADER.match(lines[i]):
            j = i + 1
            sql = []
            while j < len(lines) and lines[j].strip() != "" and lines[j].strip() != "----":
                sql.append(lines[j])
                j += 1
            if sql:
                yield "\n".join(sql)
            i = j
        else:
            i += 1


def main():
    ref = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else M.DEFAULT_REF
    out = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else M.CRATE.parent.parent / "target" / "pg-corpus" / "corpus.txt"
    seen = set()
    records = []
    files = sorted(p for p in (ref / "test").rglob("*") if p.is_file() and re.search(r"\.test(_slow|_coverage)?$", p.name))
    for path in files:
        for sql in blocks(path):
            if "${" in sql:
                continue
            sql = sql.replace("__TEST_DIR__", "/tmp/duckdb_test_dir")
            if sql in seen:
                continue
            seen.add(sql)
            records.append(sql)
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "wb") as f:
        for sql in records:
            data = sql.encode("utf-8", errors="surrogateescape")
            f.write(f"{len(data)}\n".encode())
            f.write(data)
    print(f"{len(records)} statements from {len(files)} files -> {out}")


if __name__ == "__main__":
    main()
