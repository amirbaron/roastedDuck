"""The C-to-Rust type model shared by `gen_nodes.py` and `translate_actions.py`.

Maps libpg_query's node structs, enums and field types onto the Rust types in
`src/nodes`.  Struct and field selection is driven by what the grammar actions
actually build: fields no action ever writes are dropped.
"""

from __future__ import annotations

import json
import pathlib
import re
from dataclasses import dataclass

import pgheaders

HERE = pathlib.Path(__file__).resolve().parent
CRATE = HERE.parent
DEFAULT_REF = pathlib.Path(
    "/Users/amirbaron/conductor/workspaces/duckdb-rust/ottawa/.context/duckdb-download/duckdb-1.5.6")

# Node structs the grammar builds (directly or through helpers), in output order.
NODE_STRUCTS = [
    # statements
    "PGRawStmt", "PGSelectStmt", "PGInsertStmt", "PGUpdateStmt", "PGDeleteStmt", "PGMergeIntoStmt",
    "PGMatchAction", "PGCreateStmt", "PGCreateTableAsStmt", "PGCreateSchemaStmt", "PGCreateSeqStmt",
    "PGAlterSeqStmt", "PGCreateFunctionStmt", "PGFunctionDefinition", "PGFunctionParameter",
    "PGCreateTypeStmt", "PGCreateSecretStmt", "PGDropSecretStmt", "PGViewStmt", "PGIndexStmt",
    "PGAlterTableStmt", "PGAlterTableCmd", "PGAlterObjectSchemaStmt", "PGAlterDatabaseStmt",
    "PGRenameStmt", "PGDropStmt", "PGCommentOnStmt", "PGCopyStmt", "PGCopyDatabaseStmt",
    "PGExplainStmt", "PGExportStmt", "PGImportStmt", "PGPrepareStmt", "PGExecuteStmt",
    "PGDeallocateStmt", "PGCallStmt", "PGPragmaStmt", "PGTransactionStmt", "PGVariableSetStmt",
    "PGVariableShowStmt", "PGVariableShowSelectStmt", "PGVacuumStmt", "PGLoadStmt",
    "PGUpdateExtensionsStmt", "PGAttachStmt", "PGDetachStmt", "PGUseStmt", "PGCheckPointStmt",
    "PGPivotStmt",
    # clauses and helpers
    "PGAlias", "PGRangeVar", "PGRangeSubselect", "PGRangeFunction", "PGJoinExpr", "PGWithClause",
    "PGCommonTableExpr", "PGIntoClause", "PGOnConflictClause", "PGInferClause", "PGIndexElem",
    "PGLockingClause", "PGSortBy", "PGWindowDef", "PGGroupingSet", "PGSampleSize",
    "PGSampleOptions", "PGLimitPercent", "PGAtClause", "PGPivot", "PGPivotExpr", "PGResTarget",
    "PGMultiAssignRef", "PGDefElem", "PGColumnDef", "PGConstraint", "PGCollateClause",
    "PGTableLikeClause", "PGTypeName",
    # expressions
    "PGAExpr", "PGColumnRef", "PGParamRef", "PGAConst", "PGIntervalConstant", "PGTypeCast",
    "PGFuncCall", "PGAStar", "PGAIndices", "PGAIndirection", "PGBoolExpr", "PGSubLink",
    "PGCaseExpr", "PGCaseWhen", "PGCoalesceExpr", "PGNullTest", "PGBooleanTest", "PGSetToDefault",
    "PGNamedArgExpr", "PGGroupingFunc", "PGLambdaFunction", "PGSingleArrowFunction",
    "PGPositionalReference",
]

RUST_KEYWORDS = {"type", "override", "match", "ref", "mod", "move", "box", "use", "where", "in", "fn", "loop",
                 "impl", "self", "super", "crate", "as", "break", "const", "continue", "else", "enum", "extern",
                 "false", "for", "if", "let", "mut", "pub", "return", "static", "struct", "trait", "true",
                 "unsafe", "while", "async", "await", "dyn", "abstract", "become", "do", "final", "macro",
                 "priv", "typeof", "unsized", "virtual", "yield", "try"}

# Rust names for fields whose snake_case form is unfortunate.
FIELD_RENAMES = {
    ("PGInsertStmt", "override"): "overriding",
}

# Enum fields stored in `char` columns.
CHAR_ENUM_FIELDS = {}

ENUMS = [
    "PGAExpr_Kind", "PGAlterDatabaseType", "PGAlterTableType", "PGBoolExprType", "PGBoolTestType",
    "PGCTEMaterialize", "ColumnCategory", "PGConstrType", "PGDefElemAction", "PGDropBehavior",
    "GroupingSetKind", "PGIgnoreNulls", "PGInsertColumnOrder", "PGJoinRefType", "PGJoinType",
    "PGLoadInstallType", "PGLockClauseStrength", "PGLockWaitPolicy", "PGMergeAction",
    "PGMergeActionType", "PGNewTypeKind", "PGNullTestType", "PGObjectType", "PGOnCommitAction",
    "PGOnConflictAction", "PGOnConflictActionAlias", "PGOnCreateConflict", "PGOverridingKind",
    "PGPragmaKind", "PGSetOperation", "PGSortByDir", "PGSortByNulls", "PGSubLinkType",
    "PGTransactionStmtKind", "PGTransactionStmtType", "PGViewCheckOption", "VariableSetKind",
    "VariableSetScope", "PGPostgresRelPersistence",
]

ENUM_RENAMES = {
    "PGAExpr_Kind": "AExprKind",
    "PGCTEMaterialize": "CteMaterialize",
    "PGPostgresRelPersistence": "RelPersistence",
}

# Variant-name overrides where prefix stripping gives a poor name.
VARIANT_RENAMES = {
    ("PGAExpr_Kind", "AEXPR_PAREN"): "Paren",
    ("PGConstrType", "PG_CONSTR_NOTNULL"): "NotNull",
}

# Complete variant lists for enums whose C item names do not share a clean prefix.
ENUM_VARIANTS = {
    "PGAlterDatabaseType": ["Rename"],
    "PGCTEMaterialize": ["Default", "Always", "Never"],
    "PGLockWaitPolicy": ["Block", "Skip", "Error"],
    "PGIgnoreNulls": ["Default", "Respect", "Ignore"],
    "PGInsertColumnOrder": ["ByPosition", "ByName"],
    "PGBoolExprType": ["And", "Or", "Not"],
    "PGSubLinkType": ["Exists", "All", "Any", "RowCompare", "Expr", "MultiExpr", "Array", "Cte"],
    "PGOnCreateConflict": ["Error", "Ignore", "Replace"],
    "PGViewCheckOption": ["None", "Local", "Cascaded"],
    "VariableSetKind": ["Value", "Default", "Current", "Multi", "Reset", "ResetAll"],
    "PGLockClauseStrength": ["None", "ForKeyShare", "ForShare", "ForNoKeyUpdate", "ForUpdate"],
}

# Fields stored as plain C `char`/ints that hold an enum value.
ENUM_TYPED_FIELDS = {
    ("PGRangeVar", "relpersistence"): "PGPostgresRelPersistence",
}


def snake(name: str) -> str:
    s = re.sub(r"(?<=[a-z0-9])([A-Z])", r"_\1", name)
    s = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", s)
    return s.lower()


def camel(name: str) -> str:
    parts = [p for p in re.split(r"_+", name) if p]
    return "".join(p[:1].upper() + p[1:].lower() for p in parts)


def struct_rust_name(cname: str) -> str:
    return cname[2:] if cname.startswith("PG") else cname


def enum_rust_name(cname: str) -> str:
    if cname in ENUM_RENAMES:
        return ENUM_RENAMES[cname]
    return cname[2:] if cname.startswith("PG") else cname


def field_rust_name(struct: str, cfield: str) -> str:
    if (struct, cfield) in FIELD_RENAMES:
        return FIELD_RENAMES[(struct, cfield)]
    n = snake(cfield)
    return n + "_" if n in RUST_KEYWORDS else n


@dataclass
class Ty:
    """A C type as seen by the translator."""
    kind: str            # node box list str cstr kw int bool u8 u32 i64 enum value cell void null limit funcalias optlist
    name: str | None = None
    nullable: bool = True

    def __repr__(self):
        return f"{self.kind}{'<' + self.name + '>' if self.name else ''}{'?' if self.nullable else ''}"


def c_type_to_ty(ctype: str, enums: set[str], structs: set[str]) -> Ty:
    ctype = ctype.replace("const ", "").replace("struct ", "").strip()
    if ctype in ("PGNode*", "PGExpr*"):
        return Ty("node")
    if ctype == "PGList*":
        return Ty("list", nullable=False)
    if ctype == "PGValue*":
        return Ty("value")
    if ctype == "PGValue":
        return Ty("value", nullable=False)
    if ctype in ("char*",):
        return Ty("str")
    if ctype == "char":
        return Ty("u8", nullable=False)
    if ctype == "bool":
        return Ty("bool", nullable=False)
    if ctype in ("int", "int32_t", "int32"):
        return Ty("int", nullable=False)
    if ctype in ("uint32_t", "PGOid", "PGIndex"):
        return Ty("u32", nullable=False)
    if ctype == "long":
        return Ty("i64", nullable=False)
    if ctype.endswith("*") and ctype[:-1] in structs:
        return Ty("box", struct_rust_name(ctype[:-1]))
    if ctype in enums:
        return Ty("enum", enum_rust_name(ctype), nullable=False)
    if ctype == "PGListCell*":
        return Ty("cell")
    raise KeyError(ctype)


def rust_type(ty: Ty) -> str:
    return {
        "node": "Option<Node>",
        "list": "List",
        "value": "Value" if not ty.nullable else "Option<Value>",
        "str": "Option<String>",
        "u8": "u8",
        "bool": "bool",
        "int": "i32",
        "u32": "u32",
        "i64": "i64",
        "box": f"Option<Box<{ty.name}>>",
        "enum": ty.name or "",
        "optlist": "Option<List>",
    }[ty.kind]


class Model:
    def __init__(self, ref: pathlib.Path = DEFAULT_REF):
        include = ref / "third_party" / "libpg_query" / "include"
        self.structs, self.enums, self.defines = pgheaders.load(include)
        self.enum_names = set(self.enums)
        self.struct_names = set(NODE_STRUCTS)
        # field type overrides that make the Rust model more precise than the C one
        self.field_overrides: dict[tuple[str, str], Ty] = {
            ("PGSelectStmt", "distinctClause"): Ty("optlist"),
        }

    def fields(self, cstruct: str):
        """(cfield, rust_name, Ty, comment) for every modelled field of a struct."""
        out = []
        for f in self.structs[cstruct].fields:
            if f.ctype in ("PGNodeTag", "PGExpr") and f.name in ("type", "xpr"):
                continue
            if (cstruct, f.name) in ENUM_TYPED_FIELDS:
                ty = Ty("enum", enum_rust_name(ENUM_TYPED_FIELDS[(cstruct, f.name)]), nullable=False)
            elif (cstruct, f.name) in self.field_overrides:
                ty = self.field_overrides[(cstruct, f.name)]
            else:
                ty = c_type_to_ty(f.ctype, self.enum_names, self.struct_names)
            out.append((f.name, field_rust_name(cstruct, f.name), ty, f.comment))
        return out

    def field(self, cstruct: str, cfield: str):
        for f in self.fields(cstruct):
            if f[0] == cfield:
                return f
        raise KeyError(f"{cstruct}.{cfield}")

    def enum_variants(self, cenum: str):
        """(c_item, rust_variant, value, comment)"""
        items = self.enums[cenum].items
        names = [k for k, _, _ in items]
        stripped = [n[3:] if n.startswith("PG_") else (n[2:] if n.startswith("PG") else n) for n in names]
        # common prefix ending in '_' (or camel-case prefix for CamelCase items)
        prefix = ""
        if len(stripped) > 1:
            p = stripped[0]
            for s in stripped[1:]:
                while not s.startswith(p):
                    p = p[:-1]
            if "_" in p:
                prefix = p[: p.rfind("_") + 1]
            elif all(re.match(r"[A-Z][a-z]", s) for s in stripped):
                # CamelCase items such as CTEMaterializeDefault / LockWaitBlock: cut at the
                # last upper-case letter of the common prefix.
                cut = max((i for i, ch in enumerate(p) if ch.isupper()), default=0)
                prefix = p[:cut] if all(len(s) > cut for s in stripped) else ""
                if not all(s[len(prefix):][:1].isupper() for s in stripped):
                    prefix = ""
        out = []
        value = -1
        for (cname, explicit, comment), s in zip(items, stripped):
            if explicit is not None:
                value = eval_c_int(explicit, {k: v for k, v, _ in [(i[0], i[1], 0) for i in out]})
            else:
                value += 1
            rest = s[len(prefix):] if s.startswith(prefix) else s
            variant = VARIANT_RENAMES.get((cenum, cname)) or (camel(rest) if "_" in rest or rest.isupper() else rest)
            if cenum in ENUM_VARIANTS:
                variant = ENUM_VARIANTS[cenum][len(out)]
            if variant[:1].isdigit():
                variant = "V" + variant
            out.append((cname, variant, value, comment))
        return out


def eval_c_int(expr: str, env) -> int:
    e = expr.replace("INT_MAX", str(2**31 - 1))
    e = re.sub(r"\b(\d+)[uUlL]+\b", r"\1", e)
    return int(eval(e, {}, {}))


if __name__ == "__main__":
    m = Model()
    for e in ENUMS:
        print(enum_rust_name(e), [(v[1], v[2]) for v in m.enum_variants(e)])
