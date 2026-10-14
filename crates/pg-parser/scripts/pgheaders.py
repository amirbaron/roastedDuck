"""Read struct and enum definitions (with their comments) from libpg_query's headers."""

from __future__ import annotations

import pathlib
import re
from dataclasses import dataclass, field

HEADERS = [
    "nodes/nodes.hpp", "nodes/primnodes.hpp", "nodes/parsenodes.hpp", "nodes/value.hpp",
    "nodes/pg_list.hpp", "nodes/lockoptions.hpp", "pg_definitions.hpp",
]


@dataclass
class Field:
    ctype: str
    name: str
    comment: str


@dataclass
class Struct:
    name: str
    fields: list[Field]
    comment: str


@dataclass
class Enum:
    name: str
    items: list[tuple[str, str | None, str]]  # (name, explicit value, comment)
    comment: str


def clean_comment(text: str) -> str:
    text = re.sub(r"^\s*/\*+|\*+/\s*$", "", text.strip())
    lines = [re.sub(r"^\s*\*\s?", "", ln).strip() for ln in text.splitlines()]
    out = " ".join(ln for ln in lines if ln and not re.fullmatch(r"-+", ln))
    return re.sub(r"\s+", " ", out).strip()


def preceding_comment(src: str, pos: int) -> str:
    """The block comment immediately before `pos` (only whitespace in between)."""
    before = src[:pos].rstrip()
    if not before.endswith("*/"):
        return ""
    start = before.rfind("/*")
    return clean_comment(before[start:])


def parse_struct_body(body: str) -> list[Field]:
    fields = []
    # Remove nested unions (PGValue) -- handled by hand.
    pos = 0
    stmts = []
    # split on ';' at depth 0 while remembering comments after each ';'
    depth = 0
    cur = ""
    i = 0
    while i < len(body):
        if body.startswith("/*", i):
            j = body.index("*/", i) + 2
            cur += body[i:j]
            i = j
            continue
        if body.startswith("//", i):
            j = body.find("\n", i)
            j = len(body) if j < 0 else j
            cur += "/*" + body[i + 2:j] + "*/"
            i = j
            continue
        c = body[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
        if c == ";" and depth == 0:
            stmts.append(cur)
            cur = ""
            # trailing comment on the same line
            m = re.match(r"[ \t]*(/\*.*?\*/|//[^\n]*)", body[i + 1:], re.S)
            if m:
                stmts[-1] += "\x00" + m.group(1)
                i += 1 + m.end()
                continue
        else:
            cur += c
        i += 1
    for st in stmts:
        decl, _, trailing = st.partition("\x00")
        lead_comments = re.findall(r"/\*.*?\*/", decl, re.S)
        code = re.sub(r"/\*.*?\*/", " ", decl, flags=re.S)
        code = " ".join(code.split())
        if not code or "{" in code:
            continue
        m = re.fullmatch(r"(.+?)\s*\b([A-Za-z_]\w*)", code)
        if not m:
            continue
        ctype = re.sub(r"\s*\*\s*", "*", m.group(1).replace("struct ", "")).strip()
        comment = clean_comment(trailing.replace("//", "/*", 1) + ("*/" if trailing.startswith("//") else "")) \
            if trailing else ""
        if not comment and lead_comments:
            comment = clean_comment(lead_comments[-1])
        fields.append(Field(ctype, m.group(2), comment))
    return fields


def load(include: pathlib.Path):
    structs: dict[str, Struct] = {}
    enums: dict[str, Enum] = {}
    defines: dict[str, str] = {}
    for h in HEADERS:
        src = (include / h).read_text()
        for m in re.finditer(r"typedef struct (\w+)\s*(?:/\*.*?\*/)?\s*\{", src, re.S):
            # find matching brace
            depth = 0
            i = m.end() - 1
            while True:
                if src[i] == "{":
                    depth += 1
                elif src[i] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                i += 1
            body = src[m.end():i]
            tail = re.match(r"\}\s*(\w+)\s*;", src[i:])
            name = tail.group(1) if tail else m.group(1)
            structs[name] = Struct(name, parse_struct_body(body), preceding_comment(src, m.start()))
        for m in re.finditer(r"typedef enum (\w*)\s*(?:/\*.*?\*/)?\s*\{(.*?)\}\s*(\w+)\s*;", src, re.S):
            body = m.group(2)
            items = []
            # Walk the body: code chunks separated by commas; a comment that follows an
            # item on the same line documents that item.
            pos = 0
            pending_code = ""
            for tok in re.finditer(r"/\*.*?\*/|//[^\n]*|,|\n|[^,/\n]+|/", body, re.S):
                t = tok.group(0)
                if t.startswith("/*") or t.startswith("//"):
                    text = clean_comment(t if t.startswith("/*") else "/*" + t[2:] + "*/")
                    if pending_code.strip():
                        continue
                    if items and items[-1][3] and not items[-1][2]:
                        items[-1] = (items[-1][0], items[-1][1], text, True)
                elif t == ",":
                    code = pending_code.strip()
                    if code:
                        k, _, v = code.partition("=")
                        items.append((k.strip(), v.strip() or None, "", True))
                    pending_code = ""
                elif t == "\n":
                    if items:
                        items[-1] = (items[-1][0], items[-1][1], items[-1][2], False)
                else:
                    pending_code += t
            code = pending_code.strip()
            if code:
                k, _, v = code.partition("=")
                items.append((k.strip(), v.strip() or None, "", False))
            enums[m.group(3)] = Enum(m.group(3), [(a, b, c) for a, b, c, _ in items], preceding_comment(src, m.start()))
        for m in re.finditer(r"#define\s+(\w+)\s+((?:[^\n]*\\\n)*[^\n]+)", src):
            defines[m.group(1)] = " ".join(re.sub(r"/\*.*?\*/", "", m.group(2).replace("\\\n", " ")).split())
    return structs, enums, defines
