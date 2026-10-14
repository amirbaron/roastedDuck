#!/usr/bin/env python3
"""Bootstrap generator for `src/nodes/structs.rs` and `src/nodes/enums.rs`.

Derives the Rust parse-node types from libpg_query's headers (`parsenodes.hpp`,
`primnodes.hpp`, ...).  Like `translate_actions.py`, this is a porting aid: the
output is checked in and maintained by hand.

    python3 crates/pg-parser/scripts/gen_nodes.py [duckdb-root]
"""

from __future__ import annotations

import pathlib
import sys
import textwrap

import model as M

# Fields the grammar never sets (or only ever sets to their zero value) and that
# DuckDB's transformer never reads.  They are Postgres planner/catalog leftovers.
DROPPED_FIELDS = {
    "PGCreateStmt": {"ofTypename", "tablespacename"},
    "PGCreateSeqStmt": {"ownerId", "for_identity"},
    "PGAlterSeqStmt": {"for_identity"},
    "PGIndexStmt": {"tableSpace", "excludeOpNames", "idxcomment", "indexOid", "oldNode", "primary",
                    "isconstraint", "deferrable", "initdeferred", "transformed"},
    "PGAlterObjectSchemaStmt": {"object"},
    "PGRenameStmt": {"object"},
    "PGDropStmt": {"concurrent"},
    "PGTransactionStmt": {"gid"},
    "PGJoinExpr": {"rtindex"},
    "PGCommonTableExpr": {"cterefcount"},
    "PGIntoClause": {"tableSpaceName", "viewQuery"},
    "PGIndexElem": {"indexcolname"},
    "PGColumnDef": {"inhcount", "is_local", "is_not_null", "is_from_type", "is_from_parent", "storage",
                    "cooked_default", "identity", "identitySequence", "collOid", "fdwoptions"},
    "PGConstraint": {"cooked_expr", "exclusions", "indexspace", "access_method", "old_conpfeqop",
                     "old_pktable_oid"},
    "PGTypeName": {"typeOid", "typemod"},
    "PGSubLink": {"subLinkId"},
    "PGCaseExpr": {"casetype", "casecollid"},
    "PGCoalesceExpr": {"coalescetype", "coalescecollid"},
    "PGSetToDefault": {"typeId", "typeMod"},
    "PGGroupingFunc": {"refs", "agglevelsup"},
}


def doc_lines(text: str, indent: str) -> list[str]:
    text = text.strip()
    if not text:
        return []
    text = text.replace("`", "'")
    if text[0].islower():
        text = text[0].upper() + text[1:]
    if not text.endswith((".", "?", "!", ")")):
        text += "."
    return [f"{indent}/// {ln}" for ln in textwrap.wrap(text, 96 - len(indent))]


def struct_doc(m: M.Model, cname: str) -> str:
    c = m.structs[cname].comment
    rust = M.struct_rust_name(cname)
    # header comments typically start with "PGFoo - description"
    if c.startswith(cname):
        c = c[len(cname):].lstrip(" -:")
    c = c.split(". ")[0] if c else ""
    return c or f"The `{cname}` node."


def kept_fields(m: M.Model, cname: str):
    return [f for f in m.fields(cname) if f[0] not in DROPPED_FIELDS.get(cname, set())]


def gen_structs(m: M.Model) -> str:
    out = [
        "//! The node structs of the Postgres parse tree.",
        "//!",
        "//! Field names follow libpg_query's C structs (`targetList` becomes `target_list`).",
        "//! Pointers become `Option`, lists become [`List`], C strings become `Option<String>`.",
        "",
        "use super::{",
        "    AExprKind, AlterDatabaseType, AlterTableType, BoolExprType, BoolTestType, ColumnCategory, ConstrType,",
        "    CteMaterialize, DefElemAction, DropBehavior, GroupingSetKind, IgnoreNulls, InsertColumnOrder,",
        "    JoinRefType, JoinType, List, LoadInstallType, LockClauseStrength, LockWaitPolicy, MergeAction,",
        "    MergeActionType, NewTypeKind, NullTestType, ObjectType, OnCommitAction, OnConflictAction,",
        "    OnConflictActionAlias, OnCreateConflict, OverridingKind, PragmaKind, RelPersistence, SetOperation,",
        "    SortByDir, SortByNulls, SubLinkType, TransactionStmtKind, TransactionStmtType, Value,",
        "    VariableSetKind, VariableSetScope, ViewCheckOption,",
        "};",
        "",
        "nodes! {",
    ]
    for cname in M.NODE_STRUCTS:
        rust = M.struct_rust_name(cname)
        out += doc_lines(f"`{cname}`: {struct_doc(m, cname)}", "    ")
        out.append(f"    {rust} {{")
        for cf, rf, ty, comment in kept_fields(m, cname):
            out += doc_lines(comment or f"`{cf}`", "        ")
            out.append(f"        {rf}: {M.rust_type(ty)},")
        out.append("    }")
    out.append("}")
    return "\n".join(out) + "\n"


def gen_enums(m: M.Model) -> str:
    out = [
        "//! Enumerations used by parse-tree nodes, mirroring libpg_query's C enums.",
        "//!",
        "//! Discriminants equal the C values, so `as i32` gives the number DuckDB's",
        "//! transformer sees.",
        "",
    ]
    for cenum in M.ENUMS:
        rname = M.enum_rust_name(cenum)
        c = m.enums[cenum].comment
        doc = c.split(". ")[0] if c else ""
        if doc.startswith(cenum):
            doc = doc[len(cenum):].lstrip(" -:")
        out += doc_lines(f"`{cenum}`{': ' + doc if doc else '.'}", "")
        out.append("#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]")
        out.append("#[repr(i32)]")
        out.append(f"pub enum {rname} {{")
        for i, (citem, variant, value, comment) in enumerate(m.enum_variants(cenum)):
            out += doc_lines(comment or f"`{citem}`", "    ")
            if i == 0:
                out.append("    #[default]")
            out.append(f"    {variant} = {value},")
        out.append("}")
        out.append("")
    return "\n".join(out)


def main():
    ref = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else M.DEFAULT_REF
    m = M.Model(ref)
    nodes = M.CRATE / "src" / "nodes"
    nodes.mkdir(parents=True, exist_ok=True)
    (nodes / "structs.rs").write_text(gen_structs(m))
    (nodes / "enums.rs").write_text(gen_enums(m))
    print("wrote src/nodes/structs.rs and src/nodes/enums.rs")


if __name__ == "__main__":
    main()
