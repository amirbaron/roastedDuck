"""Frozen byte offsets, token classes and lexer error boundaries."""
import json
from pathlib import Path
import sys

import duckdb

outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {"value": action()}
    except Exception as error:
        outcomes[name] = {"error": type(error).__name__, "message": str(error)}


queries = [
    "", "select x,1.5,'snow' from t where x >= $1 -- comment", "SELECT 雪, 'é'", "/* nested /* comment */ end */select 1",
    "-- comment\nselect 1", "select $$a;é$$", 'select "雪"."q"', "select 1::integer->'x'", "'unterminated", '"unterminated',
    "/*unterminated", "select E'\\n'", "select U&'d\\0061t'", "select 1e+", "select 0x20, 0b101", "select x := y", "select !~* x",
    "select\0 x", "select\u00a01", "\ufeffselect 1", "select 'a\0b'",
]
for index, sql in enumerate(queries):
    capture("text_" + str(index), lambda sql=sql: [(p, int(t), t.name, repr(t)) for p, t in duckdb.tokenize(sql)])
for byte in range(256):
    sql = b"select " + bytes([byte]) + b" as x"
    capture("byte_" + str(byte), lambda sql=sql: [(p, int(t)) for p, t in duckdb.tokenize(sql)])
for label, args, kwargs in [
    ("missing", (), {}), ("none", (None,), {}), ("int", (42,), {}),
    ("keyword", (), {"query": "select 1"}), ("extra", ("select 1", "select 2"), {}),
    ("duplicate", ("select 1",), {"query": "select 2"}), ("unknown", (), {"bad": "select 1"}),
    ("bytearray", (bytearray(b"select 1"),), {}),
]:
    capture(label, lambda args=args, kwargs=kwargs: [(p, int(t)) for p, t in duckdb.tokenize(*args, **kwargs)])
for value in [0,0.0,False,True,1,255,256,-1,'0',None,duckdb.StatementType.INVALID,duckdb.ExplainType.STANDARD]:
    capture('equal_'+repr(value),lambda value=value:[duckdb.token_type.identifier==value,duckdb.token_type.identifier!=value])
capture("members", lambda: [(name, int(value), repr(value), value == int(value)) for name, value in duckdb.token_type.__members__.items()])
Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes}, indent=2, ensure_ascii=False) + "\n")
print(json.dumps({"cases": len(outcomes)}))
