"""BIGNUM boundaries and real reader argument/result-lifecycle journeys."""
import json
from pathlib import Path
import re
import sys
import warnings

import duckdb

outcomes = {}
raw = {}


def normalize(value):
    if isinstance(value, dict):
        return {key: normalize(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return {type(value).__name__: [normalize(item) for item in value]}
    if isinstance(value, str):
        return re.sub(r"(DuckDBPy(?:Connection|Relation) object at )0x[0-9a-f]+", r"\1<address>", value)
    return value


def capture(name, action):
    try:
        outcomes[name] = {"value": normalize(action())}
    except Exception as error:
        raw[name] = {"error": type(error).__name__, "message": str(error)}
        outcomes[name] = normalize(raw[name])


for kind in ["connection", "relation"]:
    methods = ["to_arrow_reader", "arrow", "fetch_record_batch"]
    if kind == "relation":
        methods.append("fetch_arrow_reader")
    for method in methods:
        for label, args, kwargs in [
            ("zero", (0,), {}), ("negative", (-1,), {}), ("none", (None,), {}),
            ("float", (1.5,), {}), ("bool", (True,), {}), ("text", ("1",), {}),
            ("overflow", (2**64,), {}), ("extra", (1, 2), {}),
            ("unknown", (), {"bad": 1}), ("duplicate", (1,), {"batch_size": 1}),
        ]:
            def run(kind=kind, method=method, args=args, kwargs=kwargs):
                con = duckdb.connect()
                owner = con.execute("select 42 i") if kind == "connection" else con.sql("select 42 i")
                with warnings.catch_warnings():
                    warnings.simplefilter("ignore", DeprecationWarning)
                    reader = getattr(owner, method)(*args, **kwargs)
                return reader.read_all().to_pylist()
            capture(f"{kind}_{method}_{label}", run)

    def lifecycle(kind=kind):
        con = duckdb.connect()
        owner = con.execute("select 42 i") if kind == "connection" else con.sql("select 42 i")
        results = []
        for op in ["zero", "rows", "reader", "execute", "rows"]:
            try:
                if op == "zero":
                    owner.to_arrow_reader(0)
                elif op == "rows":
                    results.append([op, owner.fetchall()])
                elif op == "reader":
                    results.append([op, owner.to_arrow_reader().read_all().to_pylist()])
                else:
                    owner.execute("select 43 i") if kind == "connection" else owner.execute()
            except Exception as error:
                results.append([op, type(error).__name__, str(error)])
        return results
    capture(kind + "_zero_lifecycle", lifecycle)

for literal in ["0", "-1", str(2**127), str(-(2**127)), "1" + "0" * 307]:
    for consumer in ["rows", "numpy", "pandas", "arrow"]:
        def bignum(literal=literal, consumer=consumer):
            con = duckdb.connect()
            relation = con.sql("select ?::bignum x union all select NULL::bignum", params=[literal])
            if consumer == "rows":
                return relation.fetchall()
            if consumer == "numpy":
                return relation.fetchnumpy()["x"].tolist()
            if consumer == "pandas":
                return relation.df()["x"].tolist()
            table = relation.to_arrow_table()
            return [str(table.schema), con.from_arrow(table).fetchall()]
        capture("bignum_" + literal + "_" + consumer, bignum)

Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes, "raw_errors": raw}, indent=2) + "\n")
print(json.dumps({"cases": len(outcomes)}))
