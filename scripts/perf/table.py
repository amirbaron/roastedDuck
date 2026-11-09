"""Prints markdown tables from bench.py JSON results.

usage: python3 -I scripts/perf/table.py RESULT.json [MORE.json ...]       per-query table
       python3 -I scripts/perf/table.py --summary RESULT.json [...]       one line per file
With several files, the per-query table shows speed (DuckDB wall / roastedDuck wall) and memory
(roastedDuck peak RSS / DuckDB peak RSS) of each, in order (e.g. before / after).
"""
import json
import math
import os
import sys


def geomean(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float("nan")


def load(path):
    with open(path) as source:
        data = json.load(source)
    failures = [q for q, r in data["rows"].items() if r.get("check") in ("DIFF", "error")]
    if failures:
        sys.exit(f"{path}: benchmark validation failed: {', '.join(failures)}")
    rows = {q: r for q, r in data["rows"].items() if "speed" in r}
    return data, rows


def summary(paths):
    print("| run | suite | threads | DuckDB wall s | roastedDuck wall s | speed geomean | slowest | mem geomean | max mem |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    for path in paths:
        data, rows = load(path)
        tag = os.path.basename(path).split("-")[0]
        d = sum(r["duck"]["wall"] for r in rows.values())
        r_ = sum(r["rust"]["wall"] for r in rows.values())
        slow = min(rows.items(), key=lambda kv: kv[1]["speed"])
        big = max(rows.items(), key=lambda kv: kv[1]["mem"])
        print(f"| {tag} | {data['suite']} | {data['threads']} | {d:.3f} | {r_:.3f} | "
              f"{geomean([r['speed'] for r in rows.values()]):.2f}× | {slow[0]} {slow[1]['speed']:.2f}× | "
              f"{geomean([r['mem'] for r in rows.values()]):.2f}× | {big[0]} {big[1]['mem']:.2f}× |")


def per_query(paths):
    runs = [load(p) for p in paths]
    tags = [os.path.basename(p).split("-")[0] for p in paths]
    last = runs[-1][1]
    head = "| query | DuckDB ms | roastedDuck ms | DuckDB MB | roastedDuck MB |"
    sep = "|---|---:|---:|---:|---:|"
    for t in tags:
        head += f" speed {t} | mem {t} |"
        sep += "---:|---:|"
    print(head)
    print(sep)
    for q in sorted(set().union(*[set(r) for _, r in runs])):
        if q not in last:
            continue
        r = last[q]
        line = (f"| {q} | {r['duck']['wall'] * 1e3:.1f} | {r['rust']['wall'] * 1e3:.1f} | "
                f"{r['duck']['rss'] / 1e6:.0f} | {r['rust']['rss'] / 1e6:.0f} |")
        for _, rows in runs:
            x = rows.get(q)
            line += f" {x['speed']:.2f} | {x['mem']:.2f} |" if x else " | |"
        print(line)
    line = "| geomean | | | | |"
    for _, rows in runs:
        line += f" {geomean([r['speed'] for r in rows.values()]):.2f} | {geomean([r['mem'] for r in rows.values()]):.2f} |"
    print(line)


if sys.argv[1] == "--summary":
    summary(sys.argv[2:])
else:
    per_query(sys.argv[1:])
