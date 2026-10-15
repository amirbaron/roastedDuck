#!/usr/bin/env python3
"""Bootstrap translator: bison semantic actions (C) -> Rust.

Reads the action switch of DuckDB's generated grammar and writes one Rust module
per grammar source file under `src/actions/`, plus the rule dispatch table.

This is a one-shot porting aid, not a build step: its output is checked in and
maintained by hand afterwards.  Actions it cannot translate mechanically are
supplied verbatim from `scripts/manual_actions.rs.txt`.

    python3 crates/pg-parser/scripts/translate_actions.py [duckdb-root]
"""

from __future__ import annotations

import json
import pathlib
import re
import sys
from collections import defaultdict

import cparse
import gen_nodes
import model as M
from model import Ty

HERE = pathlib.Path(__file__).resolve().parent
CRATE = HERE.parent

# --------------------------------------------------------------------------------------
# Grammar loading
# --------------------------------------------------------------------------------------


def load_grammar(ref: pathlib.Path):
    pg = ref / "third_party" / "libpg_query"
    src = (pg / "src_backend_parser_gram.cpp").read_text()

    def arr(name):
        m = re.search(r"static const [\w ]+?\b" + name + r"\[\]\s*=\s*\{(.*?)\};", src, re.S)
        return [int(x) for x in re.findall(r"-?\d+", m.group(1))]

    m = re.search(r"static const char \*const yytname\[\] =\s*\{(.*?)\};", src, re.S)
    tname = [n.replace('\\"', '"').replace("\\\\", "\\") for n in re.findall(r'"((?:[^"\\]|\\.)*)"', m.group(1))]
    prhs, rhs, r1, r2 = arr("yyprhs"), arr("yyrhs"), arr("yyr1"), arr("yyr2")
    body = src[src.index("  switch (yyn)\n    {"):src.index("/* Line 1267 of yacc.c.  */")]
    parts = re.split(r"\n\s*case (\d+):\n(?=#line)", body)
    actions = {}
    for i in range(1, len(parts), 2):
        n = int(parts[i])
        b = parts[i + 1]
        fm = re.search(r'#line (\d+) "third_party/libpg_query/grammar/([^"]+)"', b)
        b = re.sub(r"#line[^\n]*\n", "", b).strip()
        b = b[:-len("break;")].strip()
        b = b[1:-2]
        b = re.sub(r"\(yyvsp\[\((\d+)\) - \((\d+)\)\]\.(\w+)\)", r"$\1.\3", b)
        b = re.sub(r"\(yylsp\[\((\d+)\) - \((\d+)\)\]\)", r"@\1", b)
        b = re.sub(r"\(yyval\.(\w+)\)", r"$$.\1", b)
        b = re.sub(r"\(yyloc\)", r"@$", b)
        actions[n] = (fm.group(2), b)
    # %type declarations
    types = {}
    for f in sorted((pg / "grammar" / "types").glob("*.yh")) + [pg / "grammar" / "grammar.y"]:
        for tm in re.finditer(r"%type\s*<(\w+)>\s*([^%]*)", f.read_text()):
            for sym in tm.group(2).split():
                types[sym] = tm.group(1)
    gy = (pg / "grammar" / "grammar.y").read_text()
    for tm in re.finditer(r"%token\s*<(\w+)>\s*([^\n]*)", gy):
        for sym in tm.group(2).split():
            types[sym] = tm.group(1)
    for stmt in (pg / "grammar" / "statements.list").read_text().split():
        types[stmt] = "node"
    for f in (pg / "grammar" / "keywords").glob("*.list"):
        for kw in f.read_text().split():
            types[kw] = "keyword"
    # source file of each nonterminal (where its first rule with an action lives)
    rules = []
    for n in range(1, len(r1)):
        i = prhs[n]
        syms = []
        while rhs[i] != -1:
            syms.append(tname[rhs[i]])
            i += 1
        rules.append({"n": n, "lhs": tname[r1[n]], "rhs": syms, "action": actions.get(n)})
    return rules, types


# --------------------------------------------------------------------------------------
# Type tables
# --------------------------------------------------------------------------------------

# union member -> (Ty, Rust accessor, SemValue constructor)
UNION = {
    "node": (Ty("node"), "node", "Node"),
    "list": (Ty("list", nullable=False), "list", "List"),
    "str": (Ty("str"), "str", "Str"),
    "keyword": (Ty("kw", nullable=False), "keyword", "Keyword"),
    "conststr": (Ty("cstr"), "const_str", "ConstStr"),
    "ival": (Ty("int", nullable=False), "int", "Int"),
    "boolean": (Ty("bool", nullable=False), "bool", "Bool"),
    "value": (Ty("value"), "value", None),
    "limit": (Ty("limit", nullable=False), "select_limit", "SelectLimit"),
    "funcalias": (Ty("funcalias", nullable=False), "func_alias", "FuncAlias"),
    "optlist": (Ty("optlist"), "opt_list", "OptList"),
    "relpersistence": (Ty("enum", "RelPersistence", nullable=False), "rel_persistence", "RelPersistence"),
}
POINTER_MEMBERS = {
    "typnam": "TypeName", "defelt": "DefElem", "sortby": "SortBy", "windef": "WindowDef",
    "jexpr": "JoinExpr", "ielem": "IndexElem", "alias": "Alias", "range": "RangeVar",
    "into": "IntoClause", "with": "WithClause", "infer": "InferClause",
    "onconflict": "OnConflictClause", "aind": "AIndices", "target": "ResTarget",
    "istmt": "InsertStmt", "vsetstmt": "VariableSetStmt",
}
ENUM_MEMBERS = {
    "jtype": "JoinType", "dbehavior": "DropBehavior", "oncommit": "OnCommitAction",
    "oncreateconflict": "OnCreateConflict", "objtype": "ObjectType", "ctematerialize": "CteMaterialize",
    "onconflictshorthand": "OnConflictActionAlias", "override": "OverridingKind",
    "sortorder": "SortByDir", "nullorder": "SortByNulls", "ignorenulls": "IgnoreNulls",
    "constr": "ConstrType", "lockstrength": "LockClauseStrength", "lockwaitpolicy": "LockWaitPolicy",
    "subquerytype": "SubLinkType", "viewcheckoption": "ViewCheckOption",
    "bynameorposition": "InsertColumnOrder", "loadinstalltype": "LoadInstallType",
    "transactiontype": "TransactionStmtType", "mergeaction": "MergeAction",
}

# Nonterminals whose Rust semantic type is more precise than the C union member.
TYPE_OVERRIDES = {
    "select_limit": "limit",
    "opt_select_limit": "limit",
    "func_alias_clause": "funcalias",
    "opt_func_arguments": "optlist",
    "distinct_clause": "optlist",
    "OptTemp": "relpersistence",
}


def snake_name(sym: str) -> str:
    s = M.snake(sym)
    return s


def member_ty(member: str) -> Ty:
    if member in UNION:
        return UNION[member][0]
    if member in POINTER_MEMBERS:
        return Ty("box", POINTER_MEMBERS[member])
    if member in ENUM_MEMBERS:
        return Ty("enum", ENUM_MEMBERS[member], nullable=False)
    raise KeyError(member)


def member_accessor(member: str) -> str:
    if member in UNION:
        return UNION[member][1]
    if member in POINTER_MEMBERS:
        return M.snake(POINTER_MEMBERS[member])
    if member in ENUM_MEMBERS:
        return M.snake(ENUM_MEMBERS[member])
    raise KeyError(member)


class Untranslatable(Exception):
    pass


# --------------------------------------------------------------------------------------
# Constants and helper functions
# --------------------------------------------------------------------------------------

def build_constants(model: M.Model):
    """C identifier -> (rust code, Ty)."""
    consts = {}
    for cenum in M.ENUMS:
        rname = M.enum_rust_name(cenum)
        for cname, variant, value, _ in model.enum_variants(cenum):
            consts[cname] = (f"{rname}::{variant}", Ty("enum", rname, nullable=False))
    frame = [k for k in model.defines if k.startswith("FRAMEOPTION_")]
    for k in frame:
        consts[k] = (f"frame_option::{k[len('FRAMEOPTION_'):]}", Ty("int", nullable=False))
    for k in ("CAS_NOT_DEFERRABLE", "CAS_DEFERRABLE", "CAS_INITIALLY_IMMEDIATE", "CAS_INITIALLY_DEFERRED",
              "CAS_NOT_VALID", "CAS_NO_INHERIT"):
        consts[k] = (f"constraint_attr::{k[4:]}", Ty("int", nullable=False))
    for k in ("PG_VACOPT_VACUUM", "PG_VACOPT_ANALYZE", "PG_VACOPT_VERBOSE", "PG_VACOPT_FREEZE", "PG_VACOPT_FULL",
              "PG_VACOPT_DISABLE_PAGE_SKIPPING"):
        consts[k] = (f"vacuum_option::{k[len('PG_VACOPT_'):]}", Ty("int", nullable=False))
    for k in ("PG_CREATE_TABLE_LIKE_DEFAULTS", "PG_CREATE_TABLE_LIKE_CONSTRAINTS", "PG_CREATE_TABLE_LIKE_IDENTITY",
              "PG_CREATE_TABLE_LIKE_INDEXES", "PG_CREATE_TABLE_LIKE_STORAGE", "PG_CREATE_TABLE_LIKE_COMMENTS",
              "PG_CREATE_TABLE_LIKE_STATISTICS", "PG_CREATE_TABLE_LIKE_ALL"):
        consts[k] = (f"table_like_option::{k[len('PG_CREATE_TABLE_LIKE_'):]}", Ty("int", nullable=False))
    for k in ("PG_FKCONSTR_ACTION_NOACTION", "PG_FKCONSTR_ACTION_RESTRICT", "PG_FKCONSTR_ACTION_CASCADE",
              "PG_FKCONSTR_ACTION_SETNULL", "PG_FKCONSTR_ACTION_SETDEFAULT", "PG_FKCONSTR_MATCH_FULL",
              "PG_FKCONSTR_MATCH_PARTIAL", "PG_FKCONSTR_MATCH_SIMPLE"):
        consts[k] = (f"foreign_key::{k[len('PG_FKCONSTR_'):]}", Ty("u8", nullable=False))
    consts["PG_ATTRIBUTE_IDENTITY_ALWAYS"] = ("identity::ALWAYS", Ty("u8", nullable=False))
    consts["ATTRIBUTE_IDENTITY_BY_DEFAULT"] = ("identity::BY_DEFAULT", Ty("u8", nullable=False))
    for k in ("MONTH", "YEAR", "DAY", "HOUR", "MINUTE", "SECOND", "MILLISECOND", "MICROSECOND", "WEEK", "DECADE",
              "CENTURY", "MILLENNIUM", "QUARTER"):
        consts[k] = (f"interval_field::{k}", Ty("int", nullable=False))
    consts["INTERVAL_FULL_RANGE"] = ("INTERVAL_FULL_RANGE", Ty("int", nullable=False))
    consts["InvalidOid"] = ("0", Ty("u32", nullable=False))
    src = (M.DEFAULT_REF / "third_party" / "libpg_query" / "src_backend_parser_gram.cpp").read_text()
    for name in re.findall(r"^\s+(\w+) = \d+,?$", src[src.index("enum yytokentype"):src.index("#define IDENT")], re.M):
        consts.setdefault(name, (f"i32::from(token::{name})", Ty("int", nullable=False)))
    consts["true"] = ("true", Ty("bool", nullable=False))
    consts["false"] = ("false", Ty("bool", nullable=False))
    consts["DEFAULT_INDEX_TYPE"] = ('"art"', Ty("cstr", nullable=False))
    return consts


# C helper -> (rust callee, [param Ty], return Ty, fallible)
# Param kinds: 'into' = impl Into<String> (non-null), 'strref' = &str
def T(kind, name=None, nullable=False):
    return Ty(kind, name, nullable)


NODE = T("node", nullable=True)
NODE_NN = T("node")
LIST = T("list")
INT = T("int")
BOOL = T("bool")
INTO = T("into")
STR = T("str", nullable=True)

FUNCS = {
    # makefuncs
    "makeAExpr": ("make_a_expr", [T("enum", "AExprKind"), LIST, NODE, NODE, INT], T("struct", "AExpr"), False),
    "makeSimpleAExpr": ("make_simple_a_expr", [T("enum", "AExprKind"), INTO, NODE, NODE, INT], T("struct", "AExpr"), False),
    "makeBoolExpr": ("make_bool_expr", [T("enum", "BoolExprType"), LIST, INT], T("struct", "BoolExpr"), False),
    "makeAlias": ("make_alias", [INTO, LIST], T("struct", "Alias"), False),
    "makeRangeVar": ("make_range_var", [STR, STR, INT], T("struct", "RangeVar"), False),
    "makeTypeName": ("make_type_name", [INTO], T("struct", "TypeName"), False),
    "makeTypeNameFromNameList": ("make_type_name_from_name_list", [LIST], T("struct", "TypeName"), False),
    "makeDefElem": ("make_def_elem", [INTO, NODE, INT], T("struct", "DefElem"), False),
    "makeDefElemExtended": ("make_def_elem_extended", [STR, INTO, NODE, T("enum", "DefElemAction"), INT],
                            T("struct", "DefElem"), False),
    "makeFuncCall": ("make_func_call", [LIST, LIST, INT], T("struct", "FuncCall"), False),
    "makeGroupingSet": ("make_grouping_set", [T("enum", "GroupingSetKind"), LIST, INT], T("struct", "GroupingSet"), False),
    "makeString": ("Value::String", [INTO], T("value"), False),
    "makeInteger": ("Value::Integer", [T("i64")], T("value"), False),
    "makeFloat": ("Value::Float", [INTO], T("value"), False),
    # gram.y helpers
    "makeColumnRef": ("make_column_ref", [INTO, LIST, INT, T("scanner")], NODE_NN, True),
    "makeTypeCast": ("make_type_cast", [NODE, T("box", "TypeName", True), INT, INT], NODE_NN, False),
    "makeStringConst": ("make_string_const", [INTO, INT], NODE_NN, False),
    "makeStringConstCast": ("make_string_const_cast", [INTO, INT, T("box", "TypeName", True)], NODE_NN, False),
    "makeSampleSize": ("make_sample_size", [NODE, BOOL], NODE_NN, False),
    "makeLimitPercent": ("make_limit_percent", [NODE], NODE_NN, False),
    "makeIntConst": ("make_int_const", [INT, INT], NODE_NN, False),
    "makeFloatConst": ("make_float_const", [INTO, INT], NODE_NN, False),
    "makeBitStringConst": ("make_bit_string_const", [INTO, INT], NODE_NN, False),
    "makeNullAConst": ("make_null_a_const", [INT], NODE_NN, False),
    "makeAConst": ("make_a_const", [T("value"), INT], NODE_NN, False),
    "makeBoolAConst": ("make_bool_a_const", [BOOL, INT], NODE_NN, False),
    "check_func_name": ("check_func_name", [LIST, T("scanner")], LIST, True),
    "check_indirection": ("check_indirection", [LIST, T("scanner")], LIST, True),
    "makeParamRef": ("make_param_ref", [INT, INT], NODE_NN, False),
    "makeNamedParamRef": ("make_named_param_ref", [INTO, INT], NODE_NN, False),
    "makeSetOp": ("make_set_op", [T("enum", "SetOperation"), BOOL, NODE, NODE], NODE_NN, False),
    "SystemFuncName": ("system_func_name", [T("strref")], LIST, False),
    "SystemTypeName": ("system_type_name", [T("strref")], T("struct", "TypeName"), False),
    "doNegate": ("do_negate", [NODE, INT], NODE_NN, False),
    "makeAndExpr": ("make_and_expr", [NODE, NODE, INT], NODE_NN, True),
    "makeOrExpr": ("make_or_expr", [NODE, NODE, INT], NODE_NN, True),
    "makeNotExpr": ("make_not_expr", [NODE, INT], NODE_NN, True),
    "makeRecursiveViewSelect": ("make_recursive_view_select", [INTO, LIST, NODE], NODE_NN, True),
    "pstrdup": (None, [INTO], T("into"), False),
    "INTERVAL_MASK": ("interval_mask", [INT], INT, False),
}


# --------------------------------------------------------------------------------------
# Expression results
# --------------------------------------------------------------------------------------

class E:
    """A translated expression: Rust code plus its Ty."""

    def __init__(self, code: str, ty: Ty, atomic: bool = True, raw: str | None = None):
        self.code = code
        self.ty = ty
        self.atomic = atomic  # can be used as a method receiver without parentheses
        self.raw = raw  # for a node made from a struct/box: the struct expression

    def paren(self) -> str:
        return self.code if self.atomic else f"({self.code})"

    def __repr__(self):
        return f"E({self.code!r}, {self.ty})"


def is_null_lit(e) -> bool:
    return e[0] == "id" and e[1] in ("NULL", "NIL")


# --------------------------------------------------------------------------------------
# The translator for one action
# --------------------------------------------------------------------------------------

class Local:
    def __init__(self, rust: str, ty: Ty, mutable: bool = False):
        self.rust = rust
        self.ty = ty
        self.mutable = mutable
        self.decl_index = None  # line index of the `let` in the output


class Action:
    def __init__(self, tr: "Translator", rule: dict, alt: int):
        self.tr = tr
        self.model = tr.model
        self.rule = rule
        self.alt = alt
        self.lines: list[str] = []
        self.indent = 0
        self.locals: dict[str, Local] = {}
        self.sv_locals: dict[int, Local] = {}   # $k bound to a local
        self.result: Local | None = None      # $$ local
        self.result_member = tr.lhs_member(rule["lhs"])
        self.uses_scanner = False
        self.returned = False

    # -- output helpers -----------------------------------------------------------------
    def emit(self, line: str) -> int:
        self.lines.append("    " * self.indent + line)
        return len(self.lines) - 1

    # -- naming ---------------------------------------------------------------------------
    def sym_name(self, k: int) -> str:
        sym = self.rule["rhs"][k - 1]
        base = sym.strip("'")
        name = M.snake(base) if re.match(r"^\w+$", base) else f"arg{k}"
        if base.isupper() or base.endswith("_P"):
            name = M.snake(base.removesuffix("_P").lower())
        if self.rule["rhs"].count(sym) > 1 or name == M.snake(self.rule["lhs"]):
            name = f"{name}{k}"
        if name in M.RUST_KEYWORDS or name in ("rhs",):
            name += "_"
        return name

    def fresh(self, base: str) -> str:
        name = M.snake(base)
        if name in M.RUST_KEYWORDS:
            name += "_"
        used = {l.rust for l in self.locals.values()} | {l.rust for l in self.sv_locals.values()}
        cand = name
        i = 2
        while cand in used:
            cand = f"{name}{i}"
            i += 1
        return cand

    # -- semantic values ------------------------------------------------------------------
    def sv_type(self, k, member: str) -> Ty:
        if k == "$":
            return member_ty(self.result_member)
        sym = self.rule["rhs"][k - 1]
        decl = self.tr.sym_member(sym)
        if decl is None:
            raise Untranslatable(f"${k} ({sym}) has no semantic type")
        return member_ty(decl)

    def sv_access(self, k: int, need_nonnull: bool) -> E:
        sym = self.rule["rhs"][k - 1]
        member = self.tr.sym_member(sym)
        ty = member_ty(member)
        acc = member_accessor(member)
        if ty.kind == "box" and need_nonnull:
            return E(f"rhs.take::<{ty.name}>({k})?", Ty("box", ty.name, nullable=False))
        if ty.kind == "str" and need_nonnull:
            return E(f"rhs.string({k})?", Ty("str", nullable=False))
        if ty.kind == "node" and need_nonnull:
            return E(f"rhs.required_node({k})?", Ty("node", nullable=False))
        if ty.kind == "box":
            return E(f"rhs.ptr::<{ty.name}>({k})?", ty)
        return E(f"rhs.{acc}({k})?", ty)

    # -- conversions ----------------------------------------------------------------------
    def coerce(self, e: E, target: Ty, ctx: str = "") -> str:
        s, t = e.ty, target
        k, tk = s.kind, t.kind
        if k == "null":
            return {
                "node": "None", "box": "None", "str": "None", "list": "List::new()", "value": "None",
                "cstr": "None", "optlist": "None",
            }.get(tk) or self.fail(f"NULL to {t}")
        if tk == "node":
            if k == "node" and e.raw is not None:
                return f"Some({e.raw}.into())" if t.nullable else e.code
            if k == "node":
                if t.nullable:
                    return e.code if s.nullable else f"Some({e.code})"
                return e.code if not s.nullable else self.fail(f"nullable node to non-null ({ctx})")
            if k == "box":
                if t.nullable:
                    return f"{e.paren()}.map(Node::from)" if s.nullable else f"Some({e.paren()}.into())"
                return f"{e.paren()}.into()" if not s.nullable else self.fail("nullable box to node")
            if k == "struct":
                return f"Some({e.paren()}.into())" if t.nullable else f"{e.paren()}.into()"
            if k == "list":
                return f"Node::from_list({e.code})" if t.nullable else f"Node::List({e.code})"
            if k == "value":
                if s.nullable:
                    return f"{e.paren()}.map(Node::Value)" if t.nullable else self.fail("nullable value")
                return f"Some(Node::Value({e.code}))" if t.nullable else f"Node::Value({e.code})"
            if k == "rawstr":
                return f"Some(Node::Str({e.code}))" if t.nullable else f"Node::Str({e.code})"
            if k in ("str", "into") and s.kind == "str":
                pass
            return self.fail(f"{s} to node")
        if tk == "box":
            if k == "box" and s.name == t.name:
                if t.nullable:
                    return e.code if s.nullable else f"Some({e.code})"
                return e.code if not s.nullable else f"required({e.code})?"
            if k == "struct" and s.name == t.name:
                return f"Some(Box::new({e.code}))" if t.nullable else f"Box::new({e.code})"
            if k == "node":
                if t.nullable:
                    return f"downcast_opt::<{t.name}>({e.code})?"
                return f"downcast::<{t.name}>({e.code})?" if not s.nullable else f"downcast::<{t.name}>(required({e.code})?)?"
            return self.fail(f"{s} to {t}")
        if tk == "list":
            if k == "list":
                return e.code
            if k == "node":
                return f"into_list({e.code})?" if s.nullable else f"into_list(Some({e.code}))?"
            return self.fail(f"{s} to list")
        if tk == "str":
            if k == "str":
                if t.nullable:
                    return e.code if s.nullable else f"Some({e.code})"
                return e.code if not s.nullable else f"required({e.code})?"
            if k in ("cstr", "into"):
                if s.nullable and k == "cstr":
                    return f"{e.paren()}.map(str::to_owned)" if t.nullable else self.fail("nullable cstr")
                return f"Some({e.paren()}.to_owned())" if t.nullable else f"{e.paren()}.to_owned()"
            if k == "kw":
                return f"Some({e.paren()}.to_owned())" if t.nullable else f"{e.paren()}.to_owned()"
            return self.fail(f"{s} to str")
        if tk == "into":
            if k == "str":
                return e.code if not s.nullable else f"required({e.code})?"
            if k in ("cstr", "kw", "into"):
                return e.code
            return self.fail(f"{s} to into")
        if tk == "strref":
            if k == "cstr" and not s.nullable:
                return e.code
            if k == "cstr":
                return f"required({e.code})?"
            if k == "str":
                return f"{e.paren()}.as_deref().unwrap_or_default()" if s.nullable else f"&{e.code}"
            if k == "kw":
                return e.code
            return self.fail(f"{s} to strref")
        if tk == "cstr":
            if k == "cstr":
                if t.nullable:
                    return e.code if s.nullable else f"Some({e.code})"
                return e.code
            return self.fail(f"{s} to cstr")
        if tk in ("int", "i64", "u8", "u32"):
            if k in ("int", "i64", "u8", "u32", "bool"):
                if k == tk:
                    return e.code
                rust = {"int": "i32", "i64": "i64", "u8": "u8", "u32": "u32"}[tk]
                if k == "bool":
                    return f"{rust}::from({e.code})"
                if (k, tk) in (("int", "i64"), ("u8", "int"), ("u8", "i64"), ("u8", "u32")):
                    return f"{rust}::from({e.code})"
                return f"{e.paren()} as {rust}"
            if k == "enum":
                return f"{e.paren()} as {'i32' if tk == 'int' else tk}"
            if k == "chr":
                return e.code
            return self.fail(f"{s} to {t}")
        if tk == "bool":
            if k == "bool":
                return e.code
            if k == "int":
                return f"{e.paren()} != 0"
            return self.fail(f"{s} to bool")
        if tk == "enum":
            if k == "enum" and s.name == t.name:
                return e.code
            if k == "int":
                return f"{t.name}::try_from({e.code})?"
            return self.fail(f"{s} to {t}")
        if tk == "value":
            if k == "value":
                if t.nullable:
                    return e.code if s.nullable else f"Some({e.code})"
                return e.code if not s.nullable else f"required({e.code})?"
            return self.fail(f"{s} to value")
        if tk == "scanner":
            return "rhs.scanner()"
        if tk in ("limit", "funcalias", "optlist") and k == tk:
            return e.code
        return self.fail(f"{s} to {t} ({ctx})")

    def fail(self, msg):
        raise Untranslatable(msg)

    # -- expressions ----------------------------------------------------------------------
    def expr(self, e, want: Ty | None = None) -> E:
        kind = e[0]
        if kind == "num":
            return E(str(e[1]), Ty("int", nullable=False))
        if kind == "str":
            return E(e[1], Ty("cstr", nullable=False))
        if kind == "chr":
            return E("b" + e[1], Ty("u8", nullable=False))
        if kind == "id":
            name = e[1]
            if name in ("NULL", "NIL"):
                return E("None", Ty("null"))
            if name in self.locals:
                loc = self.locals[name]
                return E(loc.rust, loc.ty)
            if name in self.tr.consts:
                code, ty = self.tr.consts[name]
                return E(code, ty)
            raise Untranslatable(f"unknown identifier {name}")
        if kind == "sv":
            k, member = e[1], e[2]
            if k == "$":
                if self.result is None:
                    raise Untranslatable("$$ read before assignment")
                return E(self.result.rust, self.result.ty)
            if k in self.sv_locals:
                loc = self.sv_locals[k]
                return E(loc.rust, loc.ty)
            sym = self.rule["rhs"][k - 1]
            declared = self.tr.sym_member(sym)
            if declared != member and not (member in POINTER_MEMBERS and declared in ("node",) + tuple(POINTER_MEMBERS)):
                if not (declared in ("str", "keyword") and member in ("str", "keyword")) \
                        and not (declared == "relpersistence" and member == "ival"):
                    raise Untranslatable(f"${k} read as {member} but declared {declared}")
            return self.sv_access(k, need_nonnull=False)
        if kind == "loc":
            k = e[1]
            if k == "$":
                return E("rhs.location", Ty("int", nullable=False))
            return E(f"rhs.loc({k})", Ty("int", nullable=False))
        if kind == "cast":
            ctype = e[1]
            inner = self.expr(e[2])
            if ctype in ("PGNode*", "PGExpr*"):
                if inner.ty.kind in ("struct", "box") and not inner.ty.nullable:
                    return E(f"Node::from({inner.code})", Ty("node", nullable=False), raw=inner.code)
                nullable = inner.ty.nullable or inner.ty.kind in ("list", "null")
                return E(self.coerce(inner, Ty("node", nullable=nullable)), Ty("node", nullable=nullable))
            if ctype == "PGList*":
                return E(self.coerce(inner, Ty("list", nullable=False)), Ty("list", nullable=False))
            if ctype == "char":
                return E(self.coerce(inner, Ty("u8", nullable=False)), Ty("u8", nullable=False), atomic=False)
            if ctype == "char*":
                return inner
            if ctype.endswith("*") and ctype[:-1] in self.model.struct_names:
                sname = M.struct_rust_name(ctype[:-1])
                if inner.ty.kind == "box" and inner.ty.name == sname:
                    return inner
                if inner.ty.kind == "node":
                    target = Ty("box", sname, nullable=inner.ty.nullable)
                    return E(self.coerce(inner, target), target)
            raise Untranslatable(f"cast to {ctype} from {inner.ty}")
        if kind == "call":
            return self.call(e[1], e[2], want)
        if kind == "member":
            return self.member(e)
        if kind == "unary":
            op, inner = e[1], self.expr(e[2])
            if op == "!":
                if inner.ty.kind == "bool":
                    return E(f"!{inner.paren()}", inner.ty)
                return E(self.truthy(inner, negate=True), Ty("bool", nullable=False), atomic=False)
            if op == "-":
                return E(f"-{inner.paren()}", inner.ty, atomic=False)
            if op == "+":
                return inner
            if op == "~":
                return E(f"!{inner.paren()}", inner.ty, atomic=False)
            raise Untranslatable(f"unary {op}")
        if kind == "bin":
            return self.binary(e)
        if kind == "cond":
            c = self.cond(e[1])
            a = self.expr(e[2], want)
            b = self.expr(e[3], want)
            ty = a.ty if a.ty.kind != "null" else b.ty
            if want is not None:
                ac, bc = self.coerce(a, want), self.coerce(b, want)
                ty = want
            else:
                ac, bc = a.code, b.code
            return E(f"if {c} {{ {ac} }} else {{ {bc} }}", ty, atomic=False)
        raise Untranslatable(f"expr {kind}")

    def truthy(self, e: E, negate=False) -> str:
        k = e.ty.kind
        if k == "bool":
            return f"!{e.paren()}" if negate else e.code
        if k in ("int", "u8", "u32", "i64"):
            return f"{e.paren()} {'==' if negate else '!='} 0"
        if k == "list":
            return f"{e.paren()}.is_empty()" if negate else f"!{e.paren()}.is_empty()"
        if k in ("node", "box", "str", "value", "cstr", "optlist"):
            if not e.ty.nullable:
                return "false" if negate else "true"
            return f"{e.paren()}.is_none()" if negate else f"{e.paren()}.is_some()"
        if k == "kw":
            return "false" if negate else "true"
        raise Untranslatable(f"truthiness of {e.ty}")

    def cond(self, e) -> str:
        if e[0] == "bin" and e[1] in ("&&", "||"):
            l, r = self.cond(e[2]), self.cond(e[3])
            wrap = lambda x, sub: f"({x})" if sub[0] == "bin" and sub[1] in ("&&", "||") and sub[1] != e[1] else x
            return f"{wrap(l, e[2])} {e[1]} {wrap(r, e[3])}"
        if e[0] == "bin" and e[1] in ("==", "!="):
            a, b = e[2], e[3]
            if is_null_lit(b) or is_null_lit(a):
                x = self.expr(a if is_null_lit(b) else b)
                return self.truthy(x, negate=(e[1] == "=="))
            # strcmp(...) == 0
            if a[0] == "call" and a[1] in ("strcmp", "strcasecmp") and b == ("num", 0):
                return self.strcmp(a, e[1] == "==")
            x, y = self.expr(a), self.expr(b)
            if x.ty.kind == "enum" or y.ty.kind == "enum":
                return f"{x.code} {e[1]} {y.code}"
            if x.ty.kind == "usize" and e[3][0] == "num":
                return f"{x.paren()} {e[1]} {y.code}"
            if x.ty.kind in ("int", "u8", "u32", "i64", "bool") and y.ty.kind in ("int", "u8", "u32", "i64", "bool"):
                yc = self.coerce(y, x.ty) if x.ty.kind != y.ty.kind else y.code
                return f"{x.paren()} {e[1]} {yc}"
            raise Untranslatable(f"compare {x.ty} {y.ty}")
        if e[0] == "bin" and e[1] in ("<", ">", "<=", ">="):
            x, y = self.expr(e[2]), self.expr(e[3])
            return f"{x.paren()} {e[1]} {y.paren()}"
        if e[0] == "unary" and e[1] == "!":
            inner = self.expr(e[2])
            return self.truthy(inner, negate=True)
        if e[0] == "call" and e[1] == "IsA":
            obj = self.expr(e[2][0])
            tname = e[2][1][1]
            if tname == "PGList":
                variant = "List(_)"
            else:
                variant = f"{M.struct_rust_name(tname)}(_)"
            if obj.ty.kind == "node":
                pat = f"Some(Node::{variant})" if obj.ty.nullable else f"Node::{variant}"
                return f"matches!({obj.code}, {pat})"
            raise Untranslatable("IsA on non-node")
        x = self.expr(e)
        if x.ty.kind == "int" and e[0] == "bin" and e[1] == "&":
            return f"{x.paren()} != 0"
        return self.truthy(x)

    def strcmp(self, call, equal: bool) -> str:
        a, b = self.expr(call[2][0]), self.expr(call[2][1])
        lit = b.code
        if a.ty.kind == "str":
            lhs = f"{a.paren()}.as_deref()" if a.ty.nullable else f"{a.paren()}.as_str()"
            rhs = f"Some({lit})" if a.ty.nullable else lit
        elif a.ty.kind in ("cstr", "kw"):
            lhs, rhs = (f"{a.paren()}", f"Some({lit})") if a.ty.nullable else (a.code, lit)
        else:
            raise Untranslatable("strcmp operand")
        if call[1] == "strcasecmp":
            inner = f"{a.paren()}.as_deref().unwrap_or_default()" if a.ty.nullable else a.code
            res = f"{inner}.eq_ignore_ascii_case({lit})"
            return res if equal else f"!{res}"
        return f"{lhs} {'==' if equal else '!='} {rhs}"

    def binary(self, e) -> E:
        op = e[1]
        if op in ("&&", "||", "==", "!=", "<", ">", "<=", ">="):
            return E(self.cond(e), Ty("bool", nullable=False), atomic=False)
        x, y = self.expr(e[2]), self.expr(e[3])
        if op in ("|", "&", "+", "-", "<<", ">>", "*", "/", "%"):
            # C promotes char operands to int
            if x.ty.kind == "u8" and y.ty.kind in ("int", "u8"):
                x = E(f"i32::from({x.code})", Ty("int", nullable=False))
            if y.ty.kind == "u8" and x.ty.kind == "int":
                y = E(f"i32::from({y.code})", Ty("int", nullable=False))
            yc = y.code if y.ty.kind == x.ty.kind else self.coerce(y, x.ty)
            if not y.atomic:
                yc = f"({yc})"
            return E(f"{x.paren()} {op} {yc}", x.ty, atomic=False)
        raise Untranslatable(f"binary {op}")

    def member(self, e) -> E:
        _, obj_e, fname, arrow = e
        obj = self.expr_lvalue(obj_e) if arrow else self.expr(obj_e)
        if obj.ty.kind == "value" and fname == "val":
            return E(obj.code, Ty("valunion", nullable=False))
        if obj.ty.kind == "valunion":
            if fname == "str":
                return E(f"{obj.paren()}.as_str()", Ty("cstr", nullable=True), atomic=True)
            if fname == "ival":
                return E(f"{obj.paren()}.as_int()", Ty("i64", nullable=False))
        if obj.ty.kind not in ("box", "struct"):
            raise Untranslatable(f"member {fname} of {obj.ty}")
        cstruct = "PG" + obj.ty.name
        _, rf, fty, _ = self.model.field(cstruct, fname)
        self.tr.assigned_read.add((cstruct, fname))
        code = obj.code
        if obj.ty.nullable:
            code = f"{obj.paren()}.as_deref_mut().ok_or_else(null_error)?"
        return E(f"{code}.{rf}", fty)

    def expr_lvalue(self, e) -> E:
        """An expression used as the base of `->`: must be a non-null box."""
        if e[0] == "sv" and e[1] != "$":
            k = e[1]
            if k not in self.sv_locals:
                raise Untranslatable(f"deref of unbound ${k}")
            loc = self.sv_locals[k]
            return E(loc.rust, loc.ty)
        x = self.expr(e)
        if x.ty.kind == "box" and x.ty.nullable:
            return E(f"required_mut(&mut {x.code})?", Ty("box", x.ty.name, nullable=False), atomic=True)
        return x

    def call(self, fname, args, want) -> E:
        if fname == "castNode":
            return self.expr(("cast", args[0][1] + "*", args[1]))
        if fname in ("list_make1", "list_make2", "list_make3", "list_make4"):
            items = []
            for a in args:
                x = self.expr(a)
                items.append(self.list_item(x))
            return E(f"vec![{', '.join(items)}]", Ty("list", nullable=False))
        if fname == "lappend":
            lst = self.expr(args[0])
            item = self.list_item(self.expr(args[1]))
            if lst.ty.kind != "list":
                raise Untranslatable("lappend on non-list")
            return E(f"append({self.coerce(lst, lst.ty)}, {item})", Ty("list", nullable=False))
        if fname == "lcons":
            item = self.list_item(self.expr(args[0]))
            lst = self.expr(args[1])
            return E(f"prepend({item}, {self.coerce(lst, Ty('list', nullable=False))})", Ty("list", nullable=False))
        if fname == "list_concat":
            a = self.coerce(self.expr(args[0]), Ty("list", nullable=False))
            b = self.coerce(self.expr(args[1]), Ty("list", nullable=False))
            return E(f"concat({a}, {b})", Ty("list", nullable=False))
        if fname == "list_length":
            x = self.expr(args[0])
            return E(f"{x.paren()}.len()", Ty("usize", nullable=False))
        if fname == "makeNode":
            sname = M.struct_rust_name(args[0][1])
            return E(f"{sname}::default()", Ty("struct", sname, nullable=False))
        if fname == "psprintf":
            fmt = args[0][1]
            rest = [self.expr(a) for a in args[1:]]
            return E(self.format_call(fmt, rest), Ty("str", nullable=False), atomic=True)
        if fname == "strVal":
            x = self.expr(args[0])
            return E(f"str_val({x.code})?", Ty("str", nullable=False))
        if fname in FUNCS:
            rust, params, ret, fallible = FUNCS[fname]
            if len(params) != len(args) and not (fname in ("makeColumnRef", "check_func_name", "check_indirection")):
                raise Untranslatable(f"arity {fname}")
            codes = []
            arg_tys = []
            for p, a in zip(params, args):
                if p.kind == "scanner":
                    codes.append("rhs.scanner()")
                    continue
                x = self.expr(a, p)
                arg_tys.append(x.ty)
                codes.append(self.coerce(x, p, fname))
            if rust is None:  # pstrdup
                return E(codes[0], Ty("into", nullable=False))
            call = f"{rust}({', '.join(codes)})"
            if fallible:
                call += "?"
            owned = arg_tys and arg_tys[0].kind == "str"
            if fname == "makeString":
                call = f"Value::String({codes[0]})" if owned else f"Value::String({codes[0]}.to_owned())"
            elif fname == "makeFloat":
                call = f"Value::Float({codes[0]})" if owned else f"Value::Float({codes[0]}.to_owned())"
            elif fname == "makeInteger":
                call = f"Value::Integer({codes[0]})"
            return E(call, ret if ret.kind != "value" else Ty("value", nullable=False))
        raise Untranslatable(f"call {fname}")

    def list_item(self, x: E) -> str:
        if x.ty.kind == "null":
            raise Untranslatable("NULL list element")
        m = re.fullmatch(r"rhs\.(?:node|ptr::<\w+>)\((\d+)\)\?", x.code)
        if m and x.ty.nullable:
            return f"rhs.required_node({m.group(1)})?"
        if x.ty.kind == "str":
            return self.coerce(x, Ty("str", nullable=False)).join(["Node::Str(", ")"])
        if x.ty.kind == "node" and x.ty.nullable:
            return f"required({x.code})?"
        if x.ty.kind == "box" and x.ty.nullable:
            return f"Node::from(required({x.code})?)"
        return self.coerce(x, Ty("node", nullable=False))

    def format_call(self, fmt: str, args: list[E]) -> str:
        body = fmt[1:-1]
        pieces = []
        ai = 0
        out = ""
        i = 0
        while i < len(body):
            if body[i] == "%" and body[i + 1] == "s":
                a = args[ai]
                ai += 1
                if a.ty.kind == "str" and a.ty.nullable:
                    pieces.append(f"c_str(&{a.code})")
                else:
                    pieces.append(a.code)
                out += "{}"
                i += 2
            elif body[i] in "{}":
                out += body[i] * 2
                i += 1
            else:
                out += body[i]
                i += 1
        if not pieces:
            return f'"{out}"'
        return f'format!("{out}", {", ".join(pieces)})'

    # -- statements -----------------------------------------------------------------------
    def stmts(self, stmts):
        i = 0
        while i < len(stmts):
            st = stmts[i]
            consumed = self.stmt(st, stmts[i + 1:])
            i += 1 + consumed
            if self.lines and self.lines[-1].lstrip().startswith("return Err("):
                # the rest of the block is dead code after ereport()
                self.returned = True
                return

    def block(self, st):
        saved = self.returned
        if st[0] == "block":
            self.stmts(st[1])
        else:
            self.stmts([st])
        # a return inside a branch does not end the enclosing block
        self.returned = saved

    def stmt(self, st, rest) -> int:
        kind = st[0]
        if kind == "empty" or kind == "break":
            return 0
        if kind == "block":
            self.stmts(st[1])
            return 0
        if kind == "decl":
            return self.decl(st, rest)
        if kind == "if":
            c = self.cond(st[1])
            self.emit(f"if {c} {{")
            self.indent += 1
            self.block(st[2])
            self.indent -= 1
            other = st[3]
            while other is not None and other[0] == "if":
                c2 = self.cond(other[1])
                self.emit(f"}} else if {c2} {{")
                self.indent += 1
                self.block(other[2])
                self.indent -= 1
                other = other[3]
            if other is not None:
                self.emit("} else {")
                self.indent += 1
                self.block(other)
                self.indent -= 1
            self.emit("}")
            return 0
        if kind == "expr":
            e = st[1]
            if (e[0] == "assign" and e[2][0] == "sv" and e[2][1] == "$" and e[3][0] == "call"
                    and e[3][1] == "makeNode" and self.result is None):
                return self.result_literal(e, rest)
            self.expr_stmt(e)
            return 0
        raise Untranslatable(f"statement {kind}")

    def result_literal(self, e, rest) -> int:
        """`$$.m = makeNode(T)` followed by `$$.m->field = value;` statements."""
        sname = M.struct_rust_name(e[3][2][0][1])
        fields = []
        consumed = 0
        for nxt in rest:
            if nxt[0] == "expr" and nxt[1][0] == "assign" and nxt[1][1] == "=" \
                    and nxt[1][2][0] == "member" and nxt[1][2][3] \
                    and nxt[1][2][1][0] == "sv" and nxt[1][2][1][1] == "$":
                if self.mentions_result(nxt[1][3]):
                    break
                fname = nxt[1][2][2]
                if fname in gen_nodes.DROPPED_FIELDS.get("PG" + sname, ()):
                    consumed += 1
                    continue
                _, rf, fty, _ = self.model.field("PG" + sname, fname)
                if any(f[0] == rf for f in fields):
                    break
                val = self.expr(nxt[1][3], fty)
                fields.append((rf, self.coerce(val, fty, f"{sname}.{fname}")))
                consumed += 1
            elif nxt[0] == "empty":
                consumed += 1
            else:
                break
        rust = self.result_name()
        self.result = Local(rust, Ty("struct", sname, nullable=False))
        self.result.literal = True
        self.result.decl_index = self.emit_struct_literal(rust, sname, fields)
        return consumed

    def mentions_result(self, e) -> bool:
        if isinstance(e, tuple):
            if e[0] == "sv" and e[1] == "$":
                return True
            return any(self.mentions_result(x) for x in e[1:])
        if isinstance(e, list):
            return any(self.mentions_result(x) for x in e)
        return False

    def decl(self, st, rest) -> int:
        _, ctype, name, init = st
        if init is not None and init[0] == "call" and init[1] == "castNode":
            init = ("cast", init[2][0][1] + "*", init[2][1])
        if init is None:
            raise Untranslatable("uninitialised declaration")
        if init[0] == "call" and init[1] == "makeNode":
            sname = M.struct_rust_name(init[2][0][1])
            rust = self.fresh(name)
            loc = Local(rust, Ty("struct", sname, nullable=False))
            self.locals[name] = loc
            # struct literal from directly following field assignments
            fields = []
            consumed = 0
            for nxt in rest:
                if nxt[0] == "expr" and nxt[1][0] == "assign" and nxt[1][1] == "=" \
                        and nxt[1][2][0] == "member" and nxt[1][2][1] == ("id", name) and nxt[1][2][3]:
                    if self.mentions(nxt[1][3], name):
                        break
                    fname = nxt[1][2][2]
                    if fname in gen_nodes.DROPPED_FIELDS.get("PG" + sname, ()):
                        if self.const_kind(nxt[1][3]) != "default":
                            raise Untranslatable(f"non-default store to dropped field {sname}.{fname}")
                        consumed += 1
                        continue
                    _, rf, fty, _ = self.model.field("PG" + sname, fname)
                    val = self.expr(nxt[1][3], fty)
                    if any(f[0] == rf for f in fields):
                        break
                    self.tr.assigned.add(("PG" + sname, fname, self.const_kind(nxt[1][3])))
                    fields.append((rf, self.coerce(val, fty, f"{sname}.{fname}")))
                    consumed += 1
                elif nxt[0] == "empty":
                    consumed += 1
                else:
                    break
            loc.decl_index = self.emit_struct_literal(rust, sname, fields)
            loc.literal = True
            return consumed
        ty = self.decl_type(ctype)
        if ty is None:
            val = self.expr(init)
            ty = val.ty
        else:
            val = self.expr(init, ty)
        rust = self.fresh(name)
        if ty.kind == "box" and init[0] in ("cast", "sv"):
            # a node taken over from a child: required to be present
            if init[0] == "sv":
                code = self.sv_access(init[1], need_nonnull=True).code
            else:
                inner = self.expr(init[2])
                if init[2][0] == "sv" and inner.ty.kind in ("node", "box"):
                    code = f"rhs.take::<{ty.name}>({init[2][1]})?"
                else:
                    code = f"downcast::<{ty.name}>({self.coerce(inner, Ty('node', nullable=False))})?"
            ty = Ty("box", ty.name, nullable=False)
        else:
            code = self.coerce(val, ty)
        loc = Local(rust, ty)
        self.locals[name] = loc
        loc.decl_index = self.emit(f"let {rust} = {code};")
        return 0

    def decl_type(self, ctype: str) -> Ty | None:
        if ctype == "auto":
            return None
        if ctype in ("int",):
            return Ty("int", nullable=False)
        if ctype == "char*":
            return Ty("cstr", nullable=False)
        try:
            return M.c_type_to_ty(ctype, self.model.enum_names, self.model.struct_names)
        except KeyError:
            raise Untranslatable(f"decl type {ctype}")

    def const_kind(self, e) -> str:
        if e[0] == "id" and e[1] in ("NULL", "NIL", "false", "true", "InvalidOid"):
            return "default"
        if e[0] == "num" and e[1] == 0:
            return "default"
        if e[0] == "unary" and e[1] == "-" and e[2] == ("num", 1):
            return "default"  # typemod = -1
        return "value"

    def mentions(self, e, name) -> bool:
        if isinstance(e, tuple):
            if e == ("id", name):
                return True
            return any(self.mentions(x, name) for x in e[1:])
        if isinstance(e, list):
            return any(self.mentions(x, name) for x in e)
        return False

    def emit_struct_literal(self, rust, sname, fields) -> int:
        idx = self.emit(f"let {rust} = {sname} {{")
        self.indent += 1
        for rf, code in fields:
            self.emit(f"{rf}: {code},")
        if len(fields) < len(gen_nodes.kept_fields(self.model, "PG" + sname)):
            self.emit("..Default::default()")
        self.indent -= 1
        self.emit("};")
        return idx

    def expr_stmt(self, e):
        kind = e[0]
        if kind == "assign":
            return self.assign(e)
        if kind == "call":
            f = e[1]
            if f == "ereport":
                return self.ereport(e[2])
            if f == "updateRawStmtEnd":
                raise Untranslatable("updateRawStmtEnd")
            if f in ("check_qualified_name",):
                x = self.expr(e[2][0])
                self.emit(f"check_qualified_name(&{x.code}, rhs.scanner())?;")
                return
            if f == "doNegateFloat":
                x = self.expr(e[2][0])
                self.mark_mut(e[2][0])
                self.emit(f"do_negate_float(&mut {x.code});")
                return
            if f == "insertSelectOptions":
                raise Untranslatable("insertSelectOptions")
        if kind == "postfix":
            x = self.expr(e[2])
            self.emit(f"{x.code} += 1;")
            return
        raise Untranslatable(f"expression statement {kind} {e[1] if len(e) > 1 else ''}")

    def ereport(self, args):
        level = args[0][1]
        inner = args[1]
        parts = inner[1] if inner[0] == "comma" else [inner]
        msg = None
        pos = None
        for p in parts:
            if p[0] != "call":
                raise Untranslatable("ereport part")
            if p[1] in ("errhint", "errdetail", "errmsg_internal"):
                # pg_functions.cpp throws "<fn> NOT IMPLEMENTED" from these helpers
                self.emit(f'return Err(rhs.unimplemented("{p[1]}"));')
                return
            if p[1] == "errmsg":
                fmt = p[2][0][1]
                for a in p[2][1:]:
                    if a[0] == "call" and a[1] == "NameListToString":
                        self.emit('return Err(rhs.unimplemented("NameListToString"));')
                        return
                vals = [self.expr(a) for a in p[2][1:]]
                msg = self.format_call(fmt, vals)
            elif p[1] == "parser_errposition":
                a = p[2][0]
                if a[0] == "call" and a[1] == "exprLocation":
                    self.emit('return Err(rhs.unimplemented("exprLocation"));')
                    return
                pos = self.expr(a).code
            elif p[1] == "errcode":
                pass
            else:
                raise Untranslatable(f"ereport {p[1]}")
        if msg is None:
            raise Untranslatable("ereport without message")
        if pos is None:
            self.emit(f"return Err(rhs.error({msg}));")
        else:
            self.emit(f"return Err(rhs.error_at({msg}, {pos}));")

    def assign(self, e):
        _, op, target, value = e
        # $$.member = value
        if target[0] == "sv" and target[1] == "$":
            member = target[2]
            mty = member_ty(self.result_member)
            if op != "=":
                raise Untranslatable("compound assignment to $$")
            if self.result is not None and getattr(self.result, "deferred", False):
                if value[0] == "call" and value[1] == "makeNode":
                    sname = M.struct_rust_name(value[2][0][1])
                    self.emit(f"{self.result.rust} = Box::new({sname}::default());")
                    return
                val = self.expr(value, self.result.ty)
                self.emit(f"{self.result.rust} = {self.coerce(val, self.result.ty)};")
                return
            if self.result is None:
                if value[0] == "call" and value[1] == "makeNode" and mty.kind == "box":
                    sname = M.struct_rust_name(value[2][0][1])
                    rust = self.result_name()
                    self.result = Local(rust, Ty("struct", sname, nullable=False))
                    self.result.decl_index = self.emit(f"let {rust} = {sname}::default();")
                    self.result.literal = True
                    return
                val = self.expr(value, mty)
                rust = self.result_name()
                ty = mty
                if mty.kind == "box" and val.ty.kind == "box" and not val.ty.nullable:
                    ty = val.ty
                    code = val.code
                elif mty.kind == "box" and val.ty.kind == "struct":
                    ty = val.ty
                    code = val.code
                elif mty.kind == "value" and val.ty.kind == "value":
                    ty = val.ty
                    code = val.code
                elif mty.kind == "node" and val.ty.kind == "node" and not val.ty.nullable:
                    ty = val.ty
                    code = val.code
                else:
                    code = self.coerce(val, mty)
                self.result = Local(rust, ty)
                self.result.pending = code
                self.result.pending_raw = val.raw if code == val.code else None
                self.result.decl_index = self.emit(f"let {rust} = {code};")
                return
            if getattr(self.result, "deferred", False):
                val = self.expr(value, self.result.ty)
                self.emit(f"{self.result.rust} = {self.coerce(val, self.result.ty)};")
                return
            raise Untranslatable("$$ assigned twice")
        if target[0] == "loc" and target[1] == "$":
            v = self.expr(value)
            self.emit(f"rhs.location = {v.code};")
            return
        if target[0] == "member":
            obj_e = target[1]
            fname = target[2]
            obj = self.expr_lvalue(obj_e)
            if obj.ty.kind not in ("box", "struct"):
                raise Untranslatable(f"field store on {obj.ty}")
            cstruct = "PG" + obj.ty.name
            if fname in gen_nodes.DROPPED_FIELDS.get(cstruct, ()):
                if self.const_kind(value) != "default":
                    raise Untranslatable(f"non-default store to dropped field {cstruct}.{fname}")
                return
            _, rf, fty, _ = self.model.field(cstruct, fname)
            self.tr.assigned.add((cstruct, fname, self.const_kind(value)))
            self.mark_mut(obj_e)
            if op == "=":
                val = self.expr(value, fty)
                self.emit(f"{obj.code}.{rf} = {self.coerce(val, fty, fname)};")
            elif op in ("|=", "&="):
                val = self.expr(value)
                self.emit(f"{obj.code}.{rf} {op} {self.coerce(val, fty)};")
            else:
                raise Untranslatable(f"assign op {op}")
            return
        if target[0] == "id" and target[1] in self.locals:
            loc = self.locals[target[1]]
            val = self.expr(value, loc.ty)
            loc.mutable = True
            if op == "=":
                self.emit(f"{loc.rust} = {self.coerce(val, loc.ty)};")
            else:
                self.emit(f"{loc.rust} {op} {self.coerce(val, loc.ty)};")
            return
        raise Untranslatable(f"assignment target {target[0]}")

    def mark_mut(self, obj_e):
        while obj_e[0] == "member":
            obj_e = obj_e[1]
        if obj_e[0] == "id" and obj_e[1] in self.locals:
            self.locals[obj_e[1]].mutable = True
        elif obj_e[0] == "sv":
            if obj_e[1] == "$":
                self.result.mutable = True
            else:
                self.sv_locals[obj_e[1]].mutable = True

    def result_name(self) -> str:
        name = M.snake(self.rule["lhs"])
        clash = any(M.snake(s) == name for s in self.rule["rhs"]) or name in M.RUST_KEYWORDS
        return self.fresh("result" if clash else name)

    # -- driver ---------------------------------------------------------------------------
    def prebind(self, stmts):
        """Bind $k values that are dereferenced or used more than once to locals."""
        uses = defaultdict(int)
        deref = set()

        def walk(x, under_arrow=False):
            if isinstance(x, tuple):
                if x[0] == "sv" and x[1] != "$":
                    uses[x[1]] += 1
                    if under_arrow:
                        deref.add(x[1])
                    return
                if x[0] == "member" and x[3]:
                    walk(x[1], True)
                    return
                if x[0] == "member":
                    walk(x[1], under_arrow)
                    return
                for y in x[1:]:
                    walk(y)
            elif isinstance(x, list):
                for y in x:
                    walk(y)

        walk(stmts)
        for k in sorted(uses):
            if k in deref or uses[k] > 1:
                sym = self.rule["rhs"][k - 1]
                member = self.tr.sym_member(sym)
                if member is None:
                    raise Untranslatable(f"${k} untyped")
                acc = self.sv_access(k, need_nonnull=(k in deref))
                name = self.fresh(self.sym_name(k))
                loc = Local(name, acc.ty)
                loc.decl_index = self.emit(f"let {name} = {acc.code};")
                self.sv_locals[k] = loc

    def translate(self, code: str):
        stmts = cparse.parse_action(code)
        self.prebind(stmts)
        if self.count_result_assignments(stmts) > 1 or self.result_assigned_in_branch(stmts):
            mty = member_ty(self.result_member)
            ty = Ty(mty.kind, mty.name, nullable=mty.kind not in ("box", "node"))
            if mty.kind in ("node", "box", "value"):
                ty = Ty(mty.kind, mty.name, nullable=False)
            rust = self.result_name()
            self.result = Local(rust, ty)
            self.result.deferred = True
            self.result.decl_index = self.emit(f"let {rust}: {self.rust_decl_type(ty)};")
        self.stmts(stmts)
        self.finish()

    def rust_decl_type(self, ty: Ty) -> str:
        if ty.kind == "node":
            return "Node" if not ty.nullable else "Option<Node>"
        if ty.kind == "box":
            return f"Box<{ty.name}>" if not ty.nullable else f"Option<Box<{ty.name}>>"
        if ty.kind == "value":
            return "Value"
        return M.rust_type(ty)

    def count_result_assignments(self, stmts) -> int:
        n = 0

        def walk(x):
            nonlocal n
            if isinstance(x, tuple):
                if x[0] == "assign" and x[2][0] == "sv" and x[2][1] == "$":
                    n += 1
                for y in x[1:]:
                    walk(y)
            elif isinstance(x, list):
                for y in x:
                    walk(y)
        walk(stmts)
        return n

    def result_assigned_in_branch(self, stmts) -> bool:
        for st in stmts:
            if st[0] in ("if", "switch", "foreach") and self.count_result_assignments([st]):
                return True
        return False

    def finish(self):
        if self.returned:
            for loc in list(self.locals.values()) + list(self.sv_locals.values()):
                if loc.mutable and loc.decl_index is not None:
                    self.lines[loc.decl_index] = self.lines[loc.decl_index].replace("let ", "let mut ", 1)
            return
        # mutability
        for loc in list(self.locals.values()) + list(self.sv_locals.values()) + ([self.result] if self.result else []):
            if loc.mutable and loc.decl_index is not None:
                self.lines[loc.decl_index] = self.lines[loc.decl_index].replace("let ", "let mut ", 1)
        member = self.result_member
        if self.result is None:
            if self.rule["lhs"] in self.tr.typed_syms:
                self.emit("Ok(SemValue::None)")
            else:
                self.emit("Ok(SemValue::None)")
            return
        r = self.result
        # inline `let x = expr; Ok(SemValue::..(x))` when x is used once
        val = E(r.rust, r.ty)
        ret = self.wrap_result(val, member)
        if (r.decl_index == len(self.lines) - 1 and not r.mutable and getattr(r, "pending", None)
                and not getattr(r, "literal", False)):
            code = r.pending
            self.lines.pop()
            ret = self.wrap_result(E(code, r.ty, atomic=False, raw=getattr(r, "pending_raw", None)), member)
        self.emit(f"Ok({ret})")

    def wrap_result(self, val: E, member: str) -> str:
        if member == "node" or member in POINTER_MEMBERS or member == "value":
            if val.code == "None":
                return "SemValue::Node(None)"
            if val.ty.kind == "node":
                if val.raw is not None:
                    return f"SemValue::node({val.raw})"
                return f"SemValue::Node({val.code})" if val.ty.nullable else f"SemValue::node({val.code})"
            if val.ty.kind in ("box", "struct", "value"):
                if val.ty.nullable:
                    conv = "Node::from" if val.ty.kind == "box" else "Node::Value"
                    return f"SemValue::Node({val.paren()}.map({conv}))"
                return f"SemValue::node({val.code})"
            if val.ty.kind == "list":
                return f"SemValue::Node(Node::from_list({val.code}))"
            if val.ty.kind == "null":
                return "SemValue::Node(None)"
            raise Untranslatable(f"result {val.ty} as node")
        mty = member_ty(member)
        if member in ENUM_MEMBERS:
            return f"SemValue::{ENUM_MEMBERS[member]}({self.coerce(val, mty)})"
        ctor = UNION[member][2]
        return f"SemValue::{ctor}({self.coerce(val, mty)})"


class Translator:
    def __init__(self, ref: pathlib.Path):
        self.model = M.Model(ref)
        self.rules, self.types = load_grammar(ref)
        self.consts = build_constants(self.model)
        self.assigned = set()
        self.assigned_read = set()
        self.typed_syms = set(self.types)
        self.manual = self.load_manual()

    def load_manual(self):
        path = HERE / "manual_actions.rs.txt"
        out = {}
        if not path.exists():
            return out
        cur = None
        buf = []
        for line in path.read_text().splitlines():
            m = re.match(r"^// rule (\d+)\s*$", line)
            if m:
                if cur is not None:
                    out[cur] = "\n".join(buf).strip("\n")
                cur = int(m.group(1))
                buf = []
            elif cur is not None:
                buf.append(line)
        if cur is not None:
            out[cur] = "\n".join(buf).strip("\n")
        return out

    def sym_member(self, sym: str) -> str | None:
        if sym in TYPE_OVERRIDES:
            return TYPE_OVERRIDES[sym]
        return self.types.get(sym)

    def lhs_member(self, lhs: str) -> str:
        return self.sym_member(lhs) or "none"

    def forward_index(self, r) -> int | None:
        """k if the rule's action just moves `$k` into `$$` (1 for an empty action)."""
        code = r["action"][1].strip()
        if code == "":
            return 1
        if r["n"] in self.manual:
            return None
        m = re.fullmatch(r"\$\$\.(\w+) = (?:\(PGNode \*\) ?)?\$(\d+)\.(\w+);", code)
        if not m:
            return None
        k = int(m.group(2))
        nodeish = {"node", "value"} | set(POINTER_MEMBERS)
        cls = lambda member: "node" if member in nodeish else member
        lhs = self.sym_member(r["lhs"])
        src = self.sym_member(r["rhs"][k - 1])
        return k if lhs is not None and src is not None and cls(lhs) == cls(src) else None

    def run(self):
        by_lhs = defaultdict(list)
        for r in self.rules:
            by_lhs[r["lhs"]].append(r)
        files = defaultdict(list)   # module -> [(lhs, rules)]
        rule_entry = {}
        failures = []
        for lhs, rules in by_lhs.items():
            forwards = {r["n"]: self.forward_index(r) for r in rules if r["action"]}
            if all(not r["action"] or forwards[r["n"]] is not None for r in rules):
                for r in rules:
                    rule_entry[r["n"]] = ("forward", forwards.get(r["n"], 1))
                continue
            src = next(r["action"][0] for r in rules if r["action"])
            module = module_name(src)
            fn = fn_name(lhs)
            arms = []
            for alt, r in enumerate(rules):
                if not r["action"]:
                    rule_entry[r["n"]] = ("forward", 1)
                    continue
                if forwards[r["n"]] is not None:
                    rule_entry[r["n"]] = ("forward", forwards[r["n"]])
                    continue
                rule_entry[r["n"]] = (module, fn, alt)
                code = r["action"][1]
                body = None
                if r["n"] in self.manual:
                    body = self.manual[r["n"]].splitlines()
                else:
                    act = Action(self, r, alt)
                    try:
                        if code.strip() == "":
                            act.emit("Ok(SemValue::None)")
                        else:
                            act.translate(code)
                        body = act.lines
                    except (Untranslatable, KeyError) as ex:
                        failures.append((r["n"], lhs, str(ex)))
                        body = ["// UNTRANSLATED: " + str(ex).replace("\n", " ")]
                        body += ["// " + ln for ln in code.strip().splitlines()]
                        body += [f'todo_rule!({r["n"]})']
                arms.append((alt, r, body))
            files[module].append((lhs, fn, rules, arms))
        self.write(files, rule_entry)
        return failures

    def write(self, files, rule_entry):
        out_dir = CRATE / "src" / "actions"
        out_dir.mkdir(parents=True, exist_ok=True)
        for module, fns in files.items():
            lines = [
                f"//! Semantic actions for the rules in `grammar/{source_of(module)}`.",
                "",
                "use super::prelude::*;",
                "",
            ]
            for lhs, fn, rules, arms in fns:
                lines.append("/// ```text")
                lines.append(f"/// {lhs}:")
                for i, r in enumerate(rules):
                    prod = " ".join(r["rhs"]) or "/* empty */"
                    lines.append(f"///     {'| ' if i else '  '}{prod}")
                lines.append("/// ```")
                lines.append(f"pub(super) fn {fn}<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {{")
                lines.append("    match alt {")
                groups = []
                for alt, r, body in arms:
                    for g in groups:
                        if g[2] == body:
                            g[0].append(alt)
                            g[1].append(r)
                            break
                    else:
                        groups.append(([alt], [r], body))
                for alts, rs, body in groups:
                    for r in rs:
                        prod = " ".join(r["rhs"]) or "/* empty */"
                        lines.append(f"        // {prod}")
                    pattern = alt_pattern(alts)
                    body = tidy_body(body)
                    if len(body) == 1 and body[0].startswith(("Ok(", "Err(")):
                        lines.append(f"        {pattern} => {body[0]},")
                    else:
                        lines.append(f"        {pattern} => {{")
                        for b in body:
                            lines.append("            " + b if b else "")
                        lines.append("        }")
                lines.append("        _ => Err(rhs.bad_alternative()),")
                lines.append("    }")
                lines.append("}")
                lines.append("")
            (out_dir / f"{module}.rs").write_text("\n".join(lines))
        # dispatch table
        mods = sorted(files)
        tab = [
            "//! Semantic actions: one function per nonterminal, dispatched by rule number.",
            "//!",
            "//! Bootstrapped from the action switch of DuckDB's generated grammar by",
            "//! `scripts/translate_actions.py` and maintained by hand since.",
            "",
        ]
        for m in mods:
            tab.append(f"mod {m};")
        tab += ["mod prelude;", "", "use crate::semantic::{Action, RuleAction};", "",
                "/// The action for every rule, indexed by rule number (rule 0 is bison's `$accept`).",
                f"pub(crate) static RULE_ACTIONS: [RuleAction; {len(self.rules) + 1}] = ["]
        tab.append("    RuleAction::Default,")
        for r in self.rules:
            ent = rule_entry.get(r["n"])
            if ent is None or ent == ("forward", 1):
                tab.append("    RuleAction::Default,")
            elif ent[0] == "forward":
                tab.append(f"    RuleAction::Forward({ent[1]}),")
            else:
                module, fn, alt = ent
                tab.append(f"    RuleAction::Call({module}::{fn} as Action, {alt}),")
        tab.append("];")
        tab += ["", "/// The production each entry of [`RULE_ACTIONS`] was written for; the",
                "/// `rule_actions_match_grammar` test compares it with the parse tables.",
                "#[cfg(test)]",
                f"pub(crate) static RULE_PRODUCTIONS: [&str; {len(self.rules) + 1}] = ["]
        tab.append('    "$accept: stmtblock $end",')
        for r in self.rules:
            prod = (r["lhs"] + ": " + " ".join(r["rhs"])).rstrip()
            tab.append("    " + json.dumps(prod) + ",")
        tab.append("];")
        (out_dir / "mod.rs").write_text("\n".join(tab) + "\n")


def alt_pattern(alts: list[int]) -> str:
    """`0 | 1 | 2 | 5` as `0..=2 | 5`."""
    runs = []
    for a in alts:
        if runs and runs[-1][1] == a - 1:
            runs[-1][1] = a
        else:
            runs.append([a, a])
    parts = []
    for lo, hi in runs:
        if hi - lo >= 2:
            parts.append(f"{lo}..={hi}")
        else:
            parts.extend(str(x) for x in range(lo, hi + 1))
    return " | ".join(parts)


def tidy_body(body: list[str]) -> list[str]:
    """Small clean-ups of generated arm bodies."""
    body = [ln.replace('Some("".to_owned())', "Some(String::new())") for ln in body]
    # a trailing `return Err(..);` is the arm's value
    if body and body[-1].startswith("return Err(") and body[-1].endswith(");"):
        body = body[:-1] + [body[-1][len("return "):-1]]
    if len(body) == 1 and body[0].startswith("Err("):
        body = [body[0]]
    return body


SOURCE_MODULE = {}


def module_name(src: str) -> str:
    base = pathlib.Path(src).stem
    if base == "grammar":
        base = "stmt_multi"
    if base == "use":
        base = "use_stmt"
    SOURCE_MODULE[base] = src
    return base


def source_of(module: str) -> str:
    return SOURCE_MODULE.get(module, module)


# Nonterminals whose snake_case names collide with another nonterminal's.
FN_RENAMES = {"Character": "character_type"}


def fn_name(lhs: str) -> str:
    if lhs in FN_RENAMES:
        return FN_RENAMES[lhs]
    n = M.snake(lhs)
    return n + "_" if n in M.RUST_KEYWORDS else n


def main():
    ref = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else M.DEFAULT_REF
    tr = Translator(ref)
    failures = tr.run()
    for n, lhs, msg in failures:
        print(f"rule {n} ({lhs}): {msg}")
    print(f"{len(failures)} untranslated actions")


if __name__ == "__main__":
    main()
