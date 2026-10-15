"""A small parser for the C subset used in bison semantic actions.

Produces a plain AST of tuples that `translate_actions.py` walks.  Only what the
DuckDB grammar actions actually use is supported.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

TOKEN_RE = re.compile(
    r"""
    (?P<ws>\s+|/\*.*?\*/|//[^\n]*)
  | (?P<dollar>\$\$\.\w+|\$\d+\.\w+)
  | (?P<at>@\$|@\d+)
  | (?P<num>0[xX][0-9a-fA-F]+|\d+)
  | (?P<str>"(?:[^"\\]|\\.)*")
  | (?P<chr>'(?:[^'\\]|\\.)')
  | (?P<id>[A-Za-z_]\w*(?:::\w+)*)
  | (?P<op>->|\+\+|--|<<=|>>=|<<|>>|<=|>=|==|!=|&&|\|\||\+=|-=|\*=|/=|\|=|&=|[-+*/%<>=!~&|^?:;,.(){}\[\]])
    """,
    re.S | re.X,
)


@dataclass
class Tok:
    kind: str
    text: str


def tokenize(src: str) -> list[Tok]:
    out = []
    pos = 0
    while pos < len(src):
        m = TOKEN_RE.match(src, pos)
        if not m:
            raise SyntaxError(f"bad char at {src[pos:pos + 30]!r}")
        pos = m.end()
        kind = m.lastgroup
        if kind == "ws":
            continue
        out.append(Tok(kind, m.group(kind)))
    out.append(Tok("eof", ""))
    return out


TYPE_WORDS = {
    "int", "char", "bool", "long", "unsigned", "const", "auto", "void", "int32_t", "uint32_t", "struct",
}


def is_type_name(name: str) -> bool:
    return name in TYPE_WORDS or (name.startswith("PG") and name[2:3].isupper()) or name in ("GroupingSetKind",)


# AST node constructors (tuples keep things light):
#   ('block', [stmts])
#   ('decl', ctype, name, init_expr|None)
#   ('expr', expr)
#   ('if', cond, then, else|None)
#   ('switch', expr, [(labels, [stmts])])  labels: list of int|'default'
#   ('foreach', var, list_expr, body)
#   ('break',)
#   ('empty',)
# expressions:
#   ('num', int) ('str', text) ('chr', text) ('id', name)
#   ('sv', k|'$', member)   $k.member / $$.member
#   ('loc', k|'$')          @k / @$
#   ('call', fname, [args])
#   ('member', obj, field, arrow:bool)
#   ('index', obj, idx)
#   ('cast', ctype, expr)
#   ('unary', op, expr)  ops: ! - ~ & * ++pre --pre
#   ('postfix', op, expr)
#   ('bin', op, l, r)
#   ('assign', op, target, value)
#   ('cond', c, a, b)
#   ('comma', [exprs])


class Parser:
    def __init__(self, src: str):
        self.toks = tokenize(src)
        self.i = 0

    # -- helpers -------------------------------------------------------------------
    def peek(self, k: int = 0) -> Tok:
        return self.toks[self.i + k]

    def next(self) -> Tok:
        t = self.toks[self.i]
        self.i += 1
        return t

    def accept(self, text: str) -> bool:
        if self.peek().text == text and self.peek().kind in ("op", "id"):
            self.i += 1
            return True
        return False

    def expect(self, text: str) -> None:
        t = self.next()
        if t.text != text:
            raise SyntaxError(f"expected {text!r}, got {t.text!r} near {self.context()}")

    def context(self) -> str:
        return " ".join(t.text for t in self.toks[max(0, self.i - 8):self.i + 8])

    # -- statements ----------------------------------------------------------------
    def parse_body(self) -> list:
        stmts = []
        while self.peek().kind != "eof":
            stmts.append(self.statement())
        return stmts

    def statement(self):
        t = self.peek()
        if t.text == "{":
            self.next()
            stmts = []
            while not self.accept("}"):
                stmts.append(self.statement())
            return ("block", stmts)
        if t.text == ";":
            self.next()
            return ("empty",)
        if t.kind == "id":
            if t.text == "if":
                self.next()
                self.expect("(")
                cond = self.expression()
                self.expect(")")
                then = self.statement()
                other = None
                if self.accept("else"):
                    other = self.statement()
                return ("if", cond, then, other)
            if t.text == "switch":
                self.next()
                self.expect("(")
                e = self.expression()
                self.expect(")")
                self.expect("{")
                arms = []
                while not self.accept("}"):
                    labels = []
                    while self.peek().text in ("case", "default"):
                        if self.accept("default"):
                            labels.append("default")
                        else:
                            self.next()
                            labels.append(self.expression())
                        self.expect(":")
                    body = []
                    while self.peek().text not in ("case", "default", "}"):
                        body.append(self.statement())
                    arms.append((labels, body))
                return ("switch", e, arms)
            if t.text == "foreach":
                self.next()
                self.expect("(")
                var = self.next().text
                self.expect(",")
                lst = self.expression()
                self.expect(")")
                body = self.statement()
                return ("foreach", var, lst, body)
            if t.text == "break":
                self.next()
                self.expect(";")
                return ("break",)
            if self.looks_like_decl():
                return self.declaration()
        e = self.expression()
        self.expect(";")
        return ("expr", e)

    def looks_like_decl(self) -> bool:
        t = self.peek()
        if not is_type_name(t.text):
            return False
        # `PGFoo *x =`, `PGFoo *x;`, `int x =`, `const char *x;`, `auto x =`
        j = 0
        while self.peek(j).text in TYPE_WORDS or is_type_name(self.peek(j).text):
            j += 1
        while self.peek(j).text == "*":
            j += 1
        return self.peek(j).kind == "id" and self.peek(j + 1).text in ("=", ";", ",")

    def declaration(self):
        words = []
        while self.peek().text in TYPE_WORDS or is_type_name(self.peek().text):
            words.append(self.next().text)
        stars = 0
        while self.accept("*"):
            stars += 1
        name = self.next().text
        init = None
        if self.accept("="):
            init = self.assignment()
        self.expect(";")
        base = " ".join(w for w in words if w not in ("const", "struct"))
        return ("decl", base + "*" * stars, name, init)

    # -- expressions ---------------------------------------------------------------
    def expression(self):
        return self.assignment()

    def assignment(self):
        lhs = self.conditional()
        if self.peek().kind == "op" and self.peek().text in ("=", "|=", "&=", "+=", "-="):
            op = self.next().text
            rhs = self.assignment()
            return ("assign", op, lhs, rhs)
        return lhs

    def conditional(self):
        c = self.binary(0)
        if self.accept("?"):
            a = self.assignment()
            self.expect(":")
            b = self.conditional()
            return ("cond", c, a, b)
        return c

    PREC = [
        ["||"], ["&&"], ["|"], ["^"], ["&"], ["==", "!="], ["<", ">", "<=", ">="], ["<<", ">>"],
        ["+", "-"], ["*", "/", "%"],
    ]

    def binary(self, level: int):
        if level == len(self.PREC):
            return self.unary()
        lhs = self.binary(level + 1)
        while self.peek().kind == "op" and self.peek().text in self.PREC[level]:
            op = self.next().text
            rhs = self.binary(level + 1)
            lhs = ("bin", op, lhs, rhs)
        return lhs

    def looks_like_cast(self) -> bool:
        if self.peek().text != "(":
            return False
        j = 1
        if not (self.peek(j).text in TYPE_WORDS or is_type_name(self.peek(j).text)):
            return False
        while self.peek(j).text in TYPE_WORDS or is_type_name(self.peek(j).text):
            j += 1
        while self.peek(j).text == "*":
            j += 1
        return self.peek(j).text == ")"

    def unary(self):
        t = self.peek()
        if t.kind == "op" and t.text in ("!", "-", "+", "~", "&", "*", "++", "--"):
            self.next()
            return ("unary", t.text, self.unary())
        if self.looks_like_cast():
            self.next()
            words = []
            while self.peek().text != ")" and self.peek().text != "*":
                words.append(self.next().text)
            stars = 0
            while self.accept("*"):
                stars += 1
            self.expect(")")
            base = " ".join(w for w in words if w not in ("const", "struct"))
            return ("cast", base + "*" * stars, self.unary())
        return self.postfix()

    def postfix(self):
        e = self.primary()
        while True:
            t = self.peek()
            if t.text == "(" and t.kind == "op":
                self.next()
                args = []
                if not self.accept(")"):
                    while True:
                        args.append(self.assignment())
                        if self.accept(")"):
                            break
                        self.expect(",")
                if e[0] != "id":
                    raise SyntaxError(f"call of non-identifier near {self.context()}")
                e = ("call", e[1], args)
            elif t.text in ("->", "."):
                self.next()
                e = ("member", e, self.next().text, t.text == "->")
            elif t.text == "[":
                self.next()
                idx = self.expression()
                self.expect("]")
                e = ("index", e, idx)
            elif t.text in ("++", "--"):
                self.next()
                e = ("postfix", t.text, e)
            else:
                return e

    def primary(self):
        t = self.next()
        if t.kind == "num":
            return ("num", int(t.text, 0))
        if t.kind == "str":
            text = t.text
            # adjacent string literal concatenation
            while self.peek().kind == "str":
                text = text[:-1] + self.next().text[1:]
            return ("str", text)
        if t.kind == "chr":
            return ("chr", t.text)
        if t.kind == "dollar":
            k, member = t.text[1:].split(".")
            return ("sv", k if k == "$" else int(k), member)
        if t.kind == "at":
            k = t.text[1:]
            return ("loc", k if k == "$" else int(k))
        if t.kind == "id":
            if t.text in ("reinterpret_cast", "static_cast"):
                self.expect("<")
                words = []
                while self.peek().text not in (">", "*"):
                    words.append(self.next().text)
                stars = 0
                while self.accept("*"):
                    stars += 1
                self.expect(">")
                self.expect("(")
                e = self.expression()
                self.expect(")")
                return ("cast", " ".join(words) + "*" * stars, e)
            return ("id", t.text)
        if t.text == "(":
            e = self.expression()
            if self.peek().text == ",":
                items = [e]
                while self.accept(","):
                    items.append(self.expression())
                e = ("comma", items)
            self.expect(")")
            return e
        raise SyntaxError(f"unexpected {t.text!r} near {self.context()}")


def parse_action(code: str) -> list:
    return Parser(code).parse_body()


if __name__ == "__main__":
    import json
    import sys

    rules = json.load(open(sys.argv[1]))["rules"]
    bad = 0
    for r in rules:
        a = r["action"]
        if not a:
            continue
        try:
            parse_action(a["code"])
        except SyntaxError as e:
            bad += 1
            print(r["n"], r["lhs"], e)
    print("failures:", bad)
