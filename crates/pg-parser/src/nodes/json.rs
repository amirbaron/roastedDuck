//! A canonical JSON rendering of parse trees.
//!
//! Used to diff this parser's output against DuckDB's C++ parser (whose trees are
//! dumped in the same format by `tools/pg_dump.cpp` in the crate's dev tooling).
//! Nodes render as objects whose first key is `"node"`; enums and C `char` fields
//! render as their integer codes; absent pointers as `null`.

use std::fmt::Write as _;

use super::{List, Node, Value};
use crate::RawStmt;

/// Accumulates JSON text.
#[derive(Debug, Default)]
pub struct JsonWriter {
    out: String,
    first: bool,
}

impl JsonWriter {
    fn separator(&mut self) {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
    }

    pub(crate) fn begin_node(&mut self, name: &str) {
        self.out.push_str("{\"node\":");
        self.string(name);
        self.first = false;
    }

    pub(crate) fn end_node(&mut self) {
        self.out.push('}');
        self.first = false;
    }

    pub(crate) fn field(&mut self, name: &str, value: &dyn WriteJson) {
        self.separator();
        self.string(name);
        self.out.push(':');
        self.first = true;
        value.write_json(self);
        self.first = false;
    }

    pub(crate) fn string(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(self.out, "\\u{:04x}", c as u32);
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    fn raw(&mut self, text: &str) {
        self.out.push_str(text);
    }
}

/// Values that can be rendered into a [`JsonWriter`].
pub trait WriteJson {
    /// Appends the JSON rendering of `self`.
    fn write_json(&self, out: &mut JsonWriter);
}

impl WriteJson for Node {
    fn write_json(&self, out: &mut JsonWriter) {
        self.write_json_variant(out);
    }
}

impl<T: WriteJson> WriteJson for Box<T> {
    fn write_json(&self, out: &mut JsonWriter) {
        (**self).write_json(out);
    }
}

impl<T: WriteJson> WriteJson for Option<T> {
    fn write_json(&self, out: &mut JsonWriter) {
        match self {
            Some(value) => value.write_json(out),
            None => out.raw("null"),
        }
    }
}

impl WriteJson for List {
    fn write_json(&self, out: &mut JsonWriter) {
        out.raw("[");
        for (i, node) in self.iter().enumerate() {
            if i > 0 {
                out.raw(",");
            }
            node.write_json(out);
        }
        out.raw("]");
    }
}

impl WriteJson for String {
    fn write_json(&self, out: &mut JsonWriter) {
        out.string(self);
    }
}

impl WriteJson for bool {
    fn write_json(&self, out: &mut JsonWriter) {
        out.raw(if *self { "true" } else { "false" });
    }
}

macro_rules! write_json_display {
    ($($ty:ty),*) => {$(
        impl WriteJson for $ty {
            fn write_json(&self, out: &mut JsonWriter) {
                let _ = write!(out.out, "{self}");
            }
        }
    )*};
}
write_json_display!(i32, i64, u8, u32);

impl WriteJson for Value {
    fn write_json(&self, out: &mut JsonWriter) {
        out.begin_node("Value");
        match self {
            Value::Integer(i) => out.field("ival", i),
            Value::Float(s) => out.field("float", s),
            Value::String(s) => out.field("str", s),
            Value::BitString(s) => out.field("bitstr", s),
            Value::Null => out.field("null", &true),
        }
        out.end_node();
    }
}

macro_rules! write_json_enums {
    ($($ty:ident),* $(,)?) => {$(
        impl WriteJson for super::$ty {
            fn write_json(&self, out: &mut JsonWriter) {
                (*self as i32).write_json(out);
            }
        }
    )*};
}
write_json_enums!(
    AExprKind,
    AlterDatabaseType,
    AlterTableType,
    BoolExprType,
    BoolTestType,
    CteMaterialize,
    ColumnCategory,
    ConstrType,
    DefElemAction,
    DropBehavior,
    GroupingSetKind,
    IgnoreNulls,
    InsertColumnOrder,
    JoinRefType,
    JoinType,
    LoadInstallType,
    LockClauseStrength,
    LockWaitPolicy,
    MergeAction,
    MergeActionType,
    NewTypeKind,
    NullTestType,
    ObjectType,
    OnCommitAction,
    OnConflictAction,
    OnConflictActionAlias,
    OnCreateConflict,
    OverridingKind,
    PragmaKind,
    SetOperation,
    SortByDir,
    SortByNulls,
    SubLinkType,
    TransactionStmtKind,
    TransactionStmtType,
    ViewCheckOption,
    VariableSetKind,
    VariableSetScope,
    RelPersistence,
);

/// Renders parsed statements as canonical JSON (an array of `RawStmt` objects).
pub fn to_json(stmts: &[RawStmt]) -> String {
    let mut out = JsonWriter {
        out: String::new(),
        first: true,
    };
    out.raw("[");
    for (i, stmt) in stmts.iter().enumerate() {
        if i > 0 {
            out.raw(",");
        }
        stmt.write_json(&mut out);
    }
    out.raw("]");
    out.out
}
