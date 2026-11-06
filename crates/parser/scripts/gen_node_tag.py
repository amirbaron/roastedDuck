"""Generates src/transformer/node_tag.rs: gen_node_tag.py <duckdb-1.5.6 checkout> (run from the
workspace root)."""
import re
import sys

s = open(sys.argv[1] + '/third_party/libpg_query/include/nodes/nodes.hpp').read()
m = re.search(r'typedef enum PGNodeTag \{(.*?)\} PGNodeTag;', s, re.S)
body = re.sub(r'/\*.*?\*/', '', m.group(1), flags=re.S)
body = re.sub(r'//[^\n]*', '', body)
val = -1
tags = {}
for item in body.split(','):
    item = item.strip()
    if not item:
        continue
    if '=' in item:
        n, v = item.split('=')
        n = n.strip()
        val = int(v.strip())
    else:
        n = item
        val += 1
    tags[n] = val
st = open('crates/pg-parser/src/nodes/structs.rs').read()
names = re.findall(r'^    (\w+) \{', st, re.M)
odd = {'AExpr': 'TAExpr', 'AConst': 'TAConst', 'AStar': 'TAStar', 'AIndices': 'TAIndices',
       'AIndirection': 'TAIndirection', 'AttachStmt': 'T_PGAttachStmt', 'UseStmt': 'T_PGUseStmt'}
lines = []
for n in names:
    t = tags.get('T_PG' + n)
    if t is None:
        print("missing", n)
        continue
    lines.append(f'        Node::{n}(_) => ({t}, "{odd.get(n, "T_" + n)}"),')
head = '''//! `PGNodeTag` numbers and `Transformer::NodetypeToString` names of the parse-tree nodes,
//! which appear in some of DuckDB's error messages. Generated from `nodes/nodes.hpp`.

use rustdb_pg_parser::nodes::{Node, Value};

/// The node's `PGNodeTag` value and its `NodetypeToString` name.
pub(crate) fn node_tag(node: &Node) -> (i32, &'static str) {
    match node {
'''
values = [
    ('Node::Value(Value::Integer(_))', 'T_PGInteger', 'T_Integer'),
    ('Node::Value(Value::Float(_))', 'T_PGFloat', 'T_Float'),
    ('Node::Value(Value::String(_)) | Node::Str(_)', 'T_PGString', 'T_String'),
    ('Node::Value(Value::BitString(_))', 'T_PGBitString', 'T_BitString'),
    ('Node::Value(Value::Null)', 'T_PGNull', 'T_Null'),
    ('Node::List(_)', 'T_PGList', 'T_List'),
]
vlines = [f'        {pat} => ({tags[tag]}, "{name}"),' for pat, tag, name in values]
out = head + "\n".join(vlines + lines) + "\n    }\n}\n"
open('crates/parser/src/transformer/node_tag.rs', 'w').write(out)
