#!/usr/bin/env python3
"""Derive an error-heavy corpus from the test corpus by mutating statements.

For a deterministic sample of statements it writes truncations (at several
character offsets), single-token deletions, duplications and swaps, plus a list
of hand-written lexer edge cases. Most results are syntax or lexer errors at
varied positions, which exercises error messages and positions far more than the
test suite's own statements do.

    python3 crates/pg-parser/scripts/mutate_corpus.py [input] [output]
"""

from __future__ import annotations

import pathlib
import random
import re
import sys

import model as M

TOKEN = re.compile(r"\s+|--[^\n]*|/\*.*?\*/|'(?:[^']|'')*'|\"(?:[^\"]|\"\")*\"|\$\$.*?\$\$|\w+|::|[^\w\s]", re.S)

LEXER_CASES = [
    "SELECT 1e", "SELECT 1e+", "SELECT 1.e5", "SELECT .5e-3", "SELECT 1..2", "SELECT 1_000_000",
    "SELECT 1__0", "SELECT 1_", "SELECT 0x1F", "SELECT 0b101", "SELECT 0o17", "SELECT 2147483647",
    "SELECT 2147483648", "SELECT -2147483648", "SELECT 99999999999999999999999", "SELECT 1.5.6",
    "SELECT $1, $2_0, ?3, ?", "SELECT $name", "SELECT $$abc$$", "SELECT $tag$a$b$tag$", "SELECT $a$x$b$y$a$",
    "SELECT $$unterminated", "SELECT $tag$x$ta$", "SELECT 'it''s'", "SELECT 'a' 'b'", "SELECT 'a'\n'b'",
    "SELECT 'a' -- c\n 'b'", "SELECT 'a'\n-- c\n'b'", "SELECT 'unterminated", "SELECT E'a\\nb\\t\\\\'",
    "SELECT E'\\x41'", "SELECT E'\\101'", "SELECT E'\\xZZ'", "SELECT E'\\q'", "SELECT E'\\''", "SELECT E'\\",
    "SELECT E'\\u0041'", "SELECT E'\\U00000041'", "SELECT E'\\u12'", "SELECT E'\\uD800'", "SELECT E'\\uDC00'",
    "SELECT E'\\uD800\\uDC00'", "SELECT E'\\uD800x'", "SELECT E'\\u0000'", "SELECT E'\\0'", "SELECT E'\\xFF'",
    "SELECT B'0101'", "SELECT X'1F'", "SELECT B'01", "SELECT X'1F", "SELECT B'1' '0'", "SELECT B'1''0'",
    "SELECT N'nat'", "SELECT n'x'", "SELECT U&'d\\0061t'", "SELECT U&'abc'", "SELECT U&'a' UESCAPE '!'",
    "SELECT U&'a' UESCAPE 'x'", "SELECT U&\"col\"", "SELECT U&\"\"", "SELECT U&x", "SELECT u&",
    "SELECT \"\"", "SELECT \"a\"\"b\"", "SELECT \"unterminated", "SELECT \"MixedCase\"", "SELECT MixedCase",
    "SELECT a::int", "SELECT a:::int", "SELECT a := 1", "SELECT a ->> 'x'", "SELECT a -> 'x'", "SELECT a->-b",
    "SELECT 2**3", "SELECT 2**-3", "SELECT 7//2", "SELECT a <=> b", "SELECT a != b", "SELECT a <> b",
    "SELECT a !=- b", "SELECT a @> b", "SELECT a ~~* b", "SELECT a +- b", "SELECT a */ b", "SELECT 1 +/* c */ 2",
    "SELECT 1 --c", "SELECT /* a /* nested */ b */ 1", "SELECT /* unterminated", "SELECT /*/ 1 */ 2",
    "SELECT 1 /**/", "SELECT {'a': 1}", "SELECT [1, 2]", "SELECT #1", "SELECT 1 # 2", "SELECT a.b.c.d.e",
    "SELECT * FROM a.b.c.d", "SELECT \\", "SELECT `a`", "SELECT 'é' FRM t", "SELECT 日本 FROM t",
    "SELECT x FROM t WHERE x NOT IN (1)", "SELECT 1 NOT BETWEEN 2 AND 3", "SELECT 1 nulls first",
    "SELECT * FROM t ORDER BY a NULLS FIRST", "WITH TIME ZONE", "SELECT 1 WITH ORDINALITY",
    " SELECT 1", "SELECT　1", "SELECT ' '", "", " ", ";", ";;", "SELECT 1;;SELECT 2",
    "SELECT 1; ; SELECT 2;", "-- only a comment", "/* only */", "SELECT " + "+" * 70 + " 1",
    "SELECT " + "(" * 3000 + "1" + ")" * 3000, "SELECT " + "'" + "x" * 2000, "SELECT \x01", "SELECT\x0b1",
    "SELECT 1\x00garbage", "LIMIT 1, 2", "SELECT 1 LIMIT 1, 2", "CREATE GLOBAL TEMP TABLE t(a int)",
    "(SELECT 1 ORDER BY 1) ORDER BY 1", "SELECT * FROM t AS x(a.b)", "SELECT a FROM t GROUP BY ALL",
    "SELECT INTERVAL '1' DAY", "SELECT INTERVAL 1 DAY", "SELECT INTERVAL (1) DAY TO SECOND",
    "SET TIME ZONE INTERVAL '1' YEAR", "SELECT CAST(1 AS FLOAT(0))", "SELECT CAST(1 AS FLOAT(60))",
    "SELECT varchar(1) 'x'", "SELECT foo(a := 1) 'x'", "VACUUM (foo)", "ALTER DATABASE a SET x TO b",
    "CREATE TABLE t(a int COLLATE x COLLATE y)", "CREATE TABLE t(a int, CHECK (a > 0) NO INHERIT)",
    "CREATE TABLE t(a int UNIQUE DEFERRABLE INITIALLY DEFERRED)", "SELECT x FROM t WINDOW w AS (ROWS UNBOUNDED FOLLOWING)",
    "SELECT sum(x) OVER (ROWS BETWEEN CURRENT ROW AND 1 PRECEDING) FROM t",
    "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FILTER (WHERE x > 1) FROM t",
    "SELECT count(DISTINCT x) WITHIN GROUP (ORDER BY x) FROM t", "SELECT (1,2) OVERLAPS (3)",
    "COPY t FROM PROGRAM STDIN", "CREATE RECURSIVE VIEW v(a) AS SELECT 1 WITH CHECK OPTION",
    "SELECT [x FOR x IN [1,2,3] IF x > 1]", "SELECT MAP {1: 2, 3: 4}", "SELECT 1 IN ()", "SELECT x.y(1)",
    "SELECT x.y()", "SELECT TREAT(1 AS INTEGER)", "SELECT * EXCLUDE (a.b, c) RENAME (d.e AS f) FROM t",
    "ALTER TABLE t ADD COLUMN a.b INT", "ALTER TABLE t DROP COLUMN p", "ALTER TABLE t RENAME a.b TO c",
    "CREATE MACRO m(a, b := 1) AS a + b", "CREATE MACRO m() AS TABLE SELECT 1", "PIVOT t ON a USING sum(b)",
    "UNPIVOT t ON a, b INTO NAME n VALUE v", "SELECT * FROM t TABLESAMPLE 10%", "SELECT * FROM t USING SAMPLE 10 ROWS (reservoir, 42)",
]


def tokens(sql: str) -> list[str]:
    return [m.group(0) for m in TOKEN.finditer(sql)]


def mutations(sql: str, rng: random.Random):
    chars = len(sql)
    for k in (1, 2, 3, 4):
        cut = chars * k // 5
        if 0 < cut < chars:
            yield sql[:cut]
    toks = tokens(sql)
    words = [i for i, t in enumerate(toks) if not t.isspace()]
    if len(words) >= 2:
        i = rng.choice(words)
        yield "".join(toks[:i] + toks[i + 1:])
        j = rng.choice(words)
        yield "".join(toks[:j + 1] + [" ", toks[j]] + toks[j + 1:])
        a = rng.randrange(len(words) - 1)
        x, y = words[a], words[a + 1]
        swapped = toks[:]
        swapped[x], swapped[y] = swapped[y], swapped[x]
        yield "".join(swapped)


def read_records(path: pathlib.Path) -> list[str]:
    data = path.read_bytes()
    out = []
    pos = 0
    while pos < len(data):
        nl = data.index(b"\n", pos)
        n = int(data[pos:nl])
        out.append(data[nl + 1:nl + 1 + n].decode("utf-8", errors="surrogateescape"))
        pos = nl + 1 + n
    return out


def main():
    target = M.CRATE.parent.parent / "target" / "pg-corpus"
    src = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else target / "corpus.txt"
    out = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else target / "mutations.txt"
    rng = random.Random(1556)
    records = read_records(src)
    sample = [r for i, r in enumerate(records) if i % 3 == 0]
    seen = set()
    result = []
    for sql in LEXER_CASES + [m for r in sample for m in mutations(r, rng)]:
        if sql not in seen:
            seen.add(sql)
            result.append(sql)
    with open(out, "wb") as f:
        for sql in result:
            data = sql.encode("utf-8", errors="surrogateescape")
            f.write(f"{len(data)}\n".encode())
            f.write(data)
    print(f"{len(result)} mutated statements -> {out}")


if __name__ == "__main__":
    main()
