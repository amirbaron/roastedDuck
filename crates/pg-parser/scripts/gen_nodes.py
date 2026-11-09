#!/usr/bin/env python3
"""Bootstrap generator for `src/nodes/structs.rs` and `src/nodes/enums.rs`.

Derives the Rust parse-node types from libpg_query's headers (`parsenodes.hpp`,
`primnodes.hpp`, ...).  Like `translate_actions.py`, this is a porting aid: the
output is checked in and maintained by hand.

    python3 crates/pg-parser/scripts/gen_nodes.py [duckdb-root]
"""

from __future__ import annotations

import pathlib
import re
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
    text = codify(text)
    if text[0].islower():
        text = text[0].upper() + text[1:]
    if not text.endswith((".", "?", "!", ")")):
        text += "."
    return [f"{indent}/// {ln}" for ln in textwrap.wrap(text, 96 - len(indent))]


def codify(text: str) -> str:
    """Wraps C identifiers (`snake_case`, `CamelCase`, `PGFoo`) in backticks."""
    parts = text.split("`")
    for i in range(0, len(parts), 2):  # outside existing code spans
        parts[i] = re.sub(
            r"(?<![\w.])([A-Za-z_][\w.]*\w(?:\(\))?|[A-Za-z_]\w*)",
            lambda m: f"`{m.group(1)}`" if looks_like_code(m.group(1)) else m.group(1),
            parts[i],
        )
    return "`".join(parts)


def looks_like_code(word: str) -> bool:
    return ("_" in word or "." in word.strip(".") or word.endswith("()")
            or re.search(r"[a-z][A-Z]", word) is not None or re.match(r"[A-Z]{2,}[a-z]", word) is not None)


# One-line descriptions for structs whose header comment is missing or unhelpful.
STRUCT_DOCS = {
    "PGRawStmt": "one statement of the input with its position in the query text",
    "PGSelectStmt": "a `SELECT`, `VALUES`, `PIVOT` or set operation (`UNION`, `INTERSECT`, `EXCEPT`)",
    "PGMatchAction": "one `WHEN [NOT] MATCHED ... THEN` action of `MERGE INTO`",
    "PGAlterSeqStmt": "`ALTER SEQUENCE`",
    "PGCreateFunctionStmt": "`CREATE MACRO` / `CREATE FUNCTION` with one or more overloads",
    "PGCreateTypeStmt": "`CREATE TYPE` (an enum or an alias of another type)",
    "PGViewStmt": "`CREATE VIEW`",
    "PGAlterTableCmd": "one subcommand of `ALTER TABLE`",
    "PGAlterDatabaseStmt": "`ALTER DATABASE`",
    "PGImportStmt": "`IMPORT DATABASE`",
    "PGPragmaStmt": "`PRAGMA name`, `PRAGMA name = value` or `PRAGMA name(args)`",
    "PGTransactionStmt": "`BEGIN`, `COMMIT`, `ROLLBACK` and friends",
    "PGVariableSetStmt": "`SET` and `RESET`",
    "PGVacuumStmt": "`VACUUM` and `ANALYZE`",
    "PGLoadStmt": "`LOAD` and `INSTALL` of extensions",
    "PGPivotStmt": "a `PIVOT`/`UNPIVOT` statement",
    "PGCommonTableExpr": "one common table expression of a `WITH` clause",
    "PGGroupingSet": "a grouping set of `GROUP BY` (`ROLLUP`, `CUBE`, `GROUPING SETS`, `ALL`)",
    "PGSampleOptions": "the options of `TABLESAMPLE` / `USING SAMPLE`",
    "PGPivotExpr": "a `PIVOT`/`UNPIVOT` table reference",
    "PGDefElem": "a generic `name = value` option",
    "PGConstraint": "a column or table constraint",
    "PGAExpr": "an operator expression",
    "PGBoolExpr": "an `AND`, `OR` or `NOT` expression",
    "PGSubLink": "a subquery used in an expression (`EXISTS`, `IN`, `ANY`, scalar, `ARRAY`)",
    "PGNullTest": "`IS [NOT] NULL`",
    "PGBooleanTest": "`IS [NOT] TRUE|FALSE|UNKNOWN`",
}


def struct_doc(m: M.Model, cname: str) -> str:
    if cname in STRUCT_DOCS:
        return STRUCT_DOCS[cname]
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
        "//! Field names follow `libpg_query`'s C structs (`targetList` becomes `target_list`).",
        "//! Pointers become `Option`, lists become [`List`], C strings become `Option<ByteString>`.",
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
        "use roastedduck_common::ByteString;",
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
        "//! Enumerations used by parse-tree nodes, mirroring `libpg_query`'s C enums.",
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
            out += doc_lines(f"{comment} (`{citem}`)" if comment else f"`{citem}`", "    ")
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
