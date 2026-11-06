//! Postgres parse-tree nodes as produced by DuckDB's grammar.
//!
//! The C parser builds `PGNode *` trees out of tagged structs; here a [`Node`] is an
//! enum with one boxed variant per struct. The mapping is mechanical so that the
//! transformer can be ported line by line:
//!
//! | C                           | Rust                    |
//! |-----------------------------|-------------------------|
//! | `PGNode *`, `PGExpr *`      | `Option<Node>`          |
//! | `PGFoo *`                   | `Option<Box<Foo>>`      |
//! | `PGList *` (`NIL` = empty)  | [`List`]                |
//! | `char *`                    | `Option<ByteString>`        |
//! | `PGValue` / `makeString` …  | [`Value`]               |
//! | `char *` stored in a list   | [`Node::Str`]           |

use rustdb_common::ByteString;
mod enums;
mod json;
mod teardown;

pub use enums::*;
pub use json::to_json;
pub use structs::*;
pub use teardown::drop_tree;

/// A list of nodes (`PGList *`). `NIL` is the empty list.
pub type List = Vec<Node>;

/// A literal value node (`PGValue`): `T_PGInteger`, `T_PGFloat`, `T_PGString`,
/// `T_PGBitString` or `T_PGNull`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    /// An integer that fits in 32 bits (`T_PGInteger`; stored as C `long`).
    Integer(i64),
    /// A numeric literal kept in its textual form (`T_PGFloat`).
    Float(ByteString),
    /// A string (`T_PGString`), also used for identifiers in name lists.
    String(ByteString),
    /// A bit-string literal; the text keeps its `b`/`x` prefix (`T_PGBitString`).
    BitString(ByteString),
    /// SQL `NULL` (`T_PGNull`).
    #[default]
    Null,
}

impl Value {
    /// The textual payload of a string, float or bit-string value.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Float(s) | Self::String(s) | Self::BitString(s) => Some(s),
            Self::Integer(_) | Self::Null => None,
        }
    }
}

/// Implemented by every node struct: conversion to and from [`Node`].
pub trait NodeStruct: Sized + Into<Node> {
    /// The struct name without `libpg_query`'s `PG` prefix.
    const NAME: &'static str;

    /// Extracts the struct from a node of the matching variant.
    fn from_node(node: Node) -> Result<Box<Self>, Node>;

    /// Borrows the struct from a node of the matching variant.
    fn from_node_ref(node: &Node) -> Option<&Self>;

    /// Wraps a boxed struct in its [`Node`] variant.
    fn into_node(node: Box<Self>) -> Node;
}

/// Declares the node structs together with the [`Node`] enum that wraps them.
macro_rules! nodes {
    ($(
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$field_meta:meta])* $field:ident : $ty:ty ),* $(,)?
        }
    )*) => {
        $(
            $(#[$meta])*
            #[derive(Clone, Debug, Default, PartialEq)]
            pub struct $name {
                $( $(#[$field_meta])* pub $field: $ty, )*
            }

            impl From<$name> for Node {
                fn from(node: $name) -> Node {
                    Node::$name(Box::new(node))
                }
            }

            impl From<Box<$name>> for Node {
                fn from(node: Box<$name>) -> Node {
                    Node::$name(node)
                }
            }

            impl $crate::nodes::NodeStruct for $name {
                const NAME: &'static str = stringify!($name);

                fn from_node(node: Node) -> Result<Box<Self>, Node> {
                    match node {
                        Node::$name(inner) => Ok(inner),
                        other => Err(other),
                    }
                }

                fn from_node_ref(node: &Node) -> Option<&Self> {
                    match node {
                        Node::$name(inner) => Some(inner),
                        _ => None,
                    }
                }

                fn into_node(node: Box<Self>) -> Node {
                    Node::$name(node)
                }
            }

            impl $crate::nodes::json::WriteJson for $name {
                fn write_json(&self, out: &mut $crate::nodes::json::JsonWriter) {
                    out.begin_node(stringify!($name));
                    $( out.field(stringify!($field), &self.$field); )*
                    out.end_node();
                }
            }

            impl $crate::nodes::teardown::TakeChildren for $name {
                fn take_children(&mut self, out: &mut dyn FnMut(Node)) {
                    $( $crate::nodes::teardown::TakeChildren::take_children(&mut self.$field, out); )*
                }
            }
        )*

        /// A node of the Postgres parse tree (`PGNode *`).
        #[derive(Clone, Debug, PartialEq)]
        pub enum Node {
            /// A literal value (`PGValue`).
            Value(Value),
            /// A nested list (`PGList`).
            List(List),
            /// A bare C string stored directly in a list cell (for example the column
            /// path of `ALTER TABLE ... DROP COLUMN a.b`), as opposed to a `PGString`
            /// [`Value`].
            Str(ByteString),
            $(
                #[doc = concat!("A [`", stringify!($name), "`] node.")]
                $name(Box<$name>),
            )*
        }

        impl Node {
            /// The node's struct name (`"SelectStmt"`, `"List"`, ...).
            pub fn name(&self) -> &'static str {
                match self {
                    Node::Value(_) => "Value",
                    Node::List(_) => "List",
                    Node::Str(_) => "Str",
                    $( Node::$name(_) => stringify!($name), )*
                }
            }

            pub(crate) fn write_json_variant(&self, out: &mut $crate::nodes::json::JsonWriter) {
                use $crate::nodes::json::WriteJson;
                match self {
                    Node::Value(value) => value.write_json(out),
                    Node::List(list) => list.write_json(out),
                    Node::Str(s) => out.string(s),
                    $( Node::$name(inner) => inner.write_json(out), )*
                }
            }

            /// Moves every child node out of `self`, passing each to `out`.
            pub(crate) fn take_children(&mut self, out: &mut dyn FnMut(Node)) {
                use $crate::nodes::teardown::TakeChildren;
                match self {
                    Node::Value(_) | Node::Str(_) => {}
                    Node::List(list) => list.take_children(out),
                    $( Node::$name(inner) => inner.take_children(out), )*
                }
            }
        }
    };
}

mod structs;

impl Node {
    /// A `PGString` value node.
    pub fn string(s: impl Into<ByteString>) -> Node {
        Node::Value(Value::String(s.into()))
    }

    /// Converts a list into a nullable node pointer: `NIL` becomes `None`.
    pub fn from_list(list: List) -> Option<Node> {
        if list.is_empty() {
            None
        } else {
            Some(Node::List(list))
        }
    }

    /// Reads a nullable node pointer as a list (`(PGList *) node`): `None` is `NIL`.
    pub fn into_list(node: Option<Node>) -> Result<List, Node> {
        match node {
            None => Ok(List::new()),
            Some(Node::List(list)) => Ok(list),
            Some(other) => Err(other),
        }
    }

    /// The string of a `PGString` value node (`strVal`).
    pub fn str_val(&self) -> Option<&[u8]> {
        match self {
            Node::Value(Value::String(s)) => Some(s),
            _ => None,
        }
    }

    /// Borrows the node as the given struct, if it is one.
    pub fn downcast_ref<T: NodeStruct>(&self) -> Option<&T> {
        T::from_node_ref(self)
    }
}

impl From<Value> for Node {
    fn from(value: Value) -> Node {
        Node::Value(value)
    }
}

impl From<List> for Node {
    fn from(list: List) -> Node {
        Node::List(list)
    }
}

/// Bit flags of [`WindowDef::frame_options`] (`FRAMEOPTION_*`).
pub mod frame_option {
    #![allow(missing_docs)]
    pub const NONDEFAULT: i32 = 0x00001;
    pub const RANGE: i32 = 0x00002;
    pub const ROWS: i32 = 0x00004;
    pub const GROUPS: i32 = 0x00008;
    pub const BETWEEN: i32 = 0x00010;
    pub const START_UNBOUNDED_PRECEDING: i32 = 0x00020;
    pub const END_UNBOUNDED_PRECEDING: i32 = 0x00040;
    pub const START_UNBOUNDED_FOLLOWING: i32 = 0x00080;
    pub const END_UNBOUNDED_FOLLOWING: i32 = 0x00100;
    pub const START_CURRENT_ROW: i32 = 0x00200;
    pub const END_CURRENT_ROW: i32 = 0x00400;
    pub const START_OFFSET_PRECEDING: i32 = 0x00800;
    pub const END_OFFSET_PRECEDING: i32 = 0x01000;
    pub const START_OFFSET_FOLLOWING: i32 = 0x02000;
    pub const END_OFFSET_FOLLOWING: i32 = 0x04000;
    pub const EXCLUDE_CURRENT_ROW: i32 = 0x08000;
    pub const EXCLUDE_GROUP: i32 = 0x10000;
    pub const EXCLUDE_TIES: i32 = 0x20000;
    pub const START_OFFSET: i32 = START_OFFSET_PRECEDING | START_OFFSET_FOLLOWING;
    pub const END_OFFSET: i32 = END_OFFSET_PRECEDING | END_OFFSET_FOLLOWING;
    pub const EXCLUSION: i32 = EXCLUDE_CURRENT_ROW | EXCLUDE_GROUP | EXCLUDE_TIES;
    pub const DEFAULTS: i32 = RANGE | START_UNBOUNDED_PRECEDING | END_CURRENT_ROW;
}

/// Bit flags of [`VacuumStmt::options`] (`PG_VACOPT_*`).
pub mod vacuum_option {
    #![allow(missing_docs)]
    pub const VACUUM: i32 = 1 << 0;
    pub const ANALYZE: i32 = 1 << 1;
    pub const VERBOSE: i32 = 1 << 2;
    pub const FREEZE: i32 = 1 << 3;
    pub const FULL: i32 = 1 << 4;
    pub const NOWAIT: i32 = 1 << 5;
    pub const SKIPTOAST: i32 = 1 << 6;
    pub const DISABLE_PAGE_SKIPPING: i32 = 1 << 7;
}

/// Bit flags of [`TableLikeClause::options`] (`PG_CREATE_TABLE_LIKE_*`).
pub mod table_like_option {
    #![allow(missing_docs)]
    pub const DEFAULTS: i32 = 1 << 0;
    pub const CONSTRAINTS: i32 = 1 << 1;
    pub const IDENTITY: i32 = 1 << 2;
    pub const INDEXES: i32 = 1 << 3;
    pub const STORAGE: i32 = 1 << 4;
    pub const COMMENTS: i32 = 1 << 5;
    pub const STATISTICS: i32 = 1 << 6;
    pub const ALL: i32 = i32::MAX;
}

/// Foreign-key action and match codes stored in [`Constraint`] (`PG_FKCONSTR_*`).
pub mod foreign_key {
    #![allow(missing_docs)]
    pub const ACTION_NOACTION: u8 = b'a';
    pub const ACTION_RESTRICT: u8 = b'r';
    pub const ACTION_CASCADE: u8 = b'c';
    pub const ACTION_SETNULL: u8 = b'n';
    pub const ACTION_SETDEFAULT: u8 = b'd';
    pub const MATCH_FULL: u8 = b'f';
    pub const MATCH_PARTIAL: u8 = b'p';
    pub const MATCH_SIMPLE: u8 = b's';
}

/// `GENERATED ALWAYS` / `BY DEFAULT` codes of [`Constraint::generated_when`].
pub mod identity {
    #![allow(missing_docs)]
    pub const ALWAYS: u8 = 0;
    pub const BY_DEFAULT: u8 = 1;
}

/// Interval field numbers (`utils/datetime.hpp`), used as bits of interval typmods.
pub mod interval_field {
    #![allow(missing_docs)]
    pub const MONTH: i32 = 1;
    pub const YEAR: i32 = 2;
    pub const DAY: i32 = 3;
    pub const HOUR: i32 = 10;
    pub const MINUTE: i32 = 11;
    pub const SECOND: i32 = 12;
    pub const MILLISECOND: i32 = 13;
    pub const MICROSECOND: i32 = 14;
    pub const WEEK: i32 = 24;
    pub const DECADE: i32 = 25;
    pub const CENTURY: i32 = 26;
    pub const MILLENNIUM: i32 = 27;
    pub const QUARTER: i32 = 29;
}

/// The interval typmod meaning "no field restriction" (`INTERVAL_FULL_RANGE`).
pub const INTERVAL_FULL_RANGE: i32 = 0x7FFF;

/// `INTERVAL_MASK(field)`: the typmod bit of an interval field.
pub const fn interval_mask(field: i32) -> i32 {
    1 << field
}
