//! Hand-written parsed expressions: `StarExpression`, whose in-memory form differs from
//! its serialized one, and `BoundExpression`, which only exists while binding.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::{CaseInsensitiveMap, OrderedMap};
use rustdb_serde::{DeserializeObject, Deserializer, Result, SerializeObject, Serializer};
use rustdb_types::Value;

use crate::{
    ColumnRefExpression, ConstantExpression, ExpressionType, OperatorExpression, ParsedExpression,
    ParsedExpressionKind, QualifiedColumnName,
};

impl ParsedExpression {
    /// An expression without alias or query location.
    pub const fn new(expression_type: ExpressionType, kind: ParsedExpressionKind) -> Self {
        Self {
            expression_type,
            alias: String::new(),
            query_location: None,
            kind,
        }
    }

    /// A constant (`VALUE_CONSTANT`).
    pub fn constant(value: Value) -> Self {
        Self::new(
            ExpressionType::ValueConstant,
            ParsedExpressionKind::Constant(ConstantExpression { value }),
        )
    }

    /// A column reference: `column`, `table.column`, ...
    pub fn column_ref<S: Into<String>>(names: impl IntoIterator<Item = S>) -> Self {
        Self::new(
            ExpressionType::ColumnRef,
            ParsedExpressionKind::ColumnRef(ColumnRefExpression {
                column_names: names.into_iter().map(Into::into).collect(),
            }),
        )
    }

    /// The same expression under an alias (`expr AS alias`).
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = alias.into();
        self
    }
}

/// A sub-tree of a parsed expression that the binder has already bound (DuckDB's
/// `BoundExpression`).
///
/// The binder replaces parsed nodes with their bound form in place, so that when binding
/// fails in a subquery and is retried in an enclosing query, the parts that did bind keep
/// their inner binding. The payload is the binder's own representation, which this crate
/// cannot name; it is never serialized.
#[derive(Clone)]
pub struct BoundExpression(pub Arc<dyn Any + Send + Sync>);

impl BoundExpression {
    pub fn new(payload: impl Any + Send + Sync) -> Self {
        Self(Arc::new(payload))
    }
}

impl fmt::Debug for BoundExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BoundExpression")
    }
}

impl PartialEq for BoundExpression {
    /// Bound sub-trees are only equal to themselves.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// `*`, `t.*`, `COLUMNS(...)` and their `EXCLUDE` / `REPLACE` / `RENAME` clauses
/// (DuckDB's `StarExpression`).
#[derive(Clone, Debug, PartialEq)]
pub struct StarExpression {
    /// The table in `t.*`, or empty.
    pub relation_name: String,
    /// `EXCLUDE (...)`: plain and qualified column names, in order.
    pub exclude_list: Vec<QualifiedColumnName>,
    /// `REPLACE (expr AS name, ...)`.
    pub replace_list: CaseInsensitiveMap<ParsedExpression>,
    /// `RENAME (old AS new, ...)`.
    pub rename_list: OrderedMap<QualifiedColumnName, String>,
    /// `COLUMNS(...)` rather than `*`.
    pub columns: bool,
    /// The argument of `COLUMNS(expr)`.
    pub expr: Option<Box<ParsedExpression>>,
}

impl QualifiedColumnName {
    /// An unqualified column name.
    pub fn column(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            ..Self::default()
        }
    }

    /// Whether a catalog, schema or table is given (DuckDB's `IsQualified`).
    pub fn is_qualified(&self) -> bool {
        !self.catalog.is_empty() || !self.schema.is_empty() || !self.table.is_empty()
    }
}

impl SerializeObject for StarExpression {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property_with_default(200, "relation_name", &self.relation_name)?;
        // Unqualified exclusions are written as plain names, for backwards compatibility.
        let plain: Vec<&str> = self
            .exclude_list
            .iter()
            .filter(|name| !name.is_qualified())
            .map(|name| name.column.as_str())
            .collect();
        serializer.write_list_property(201, "exclude_list", plain.len(), |serializer, i| {
            serializer.write_str(plain[i]);
            Ok(())
        })?;
        serializer.write_property_with_default(202, "replace_list", &self.replace_list)?;
        serializer.write_property_with_default(203, "columns", &self.columns)?;
        serializer.write_property_with_default(204, "expr", &self.expr)?;
        // `unpacked` (205) is no longer written.
        let qualified: Vec<QualifiedColumnName> = self
            .exclude_list
            .iter()
            .filter(|name| name.is_qualified())
            .cloned()
            .collect();
        serializer.write_property_with_default(206, "qualified_exclude_list", &qualified)?;
        serializer.write_property_with_default(207, "rename_list", &self.rename_list)
    }
}

impl StarExpression {
    /// Reads the members and whether the expression was stored as "unpacked"
    /// (`*COLUMNS(...)` before it became an `OPERATOR_UNPACK` around the star).
    fn deserialize_with_unpacked(deserializer: &mut dyn Deserializer) -> Result<(Self, bool)> {
        let relation_name = deserializer.read_property_with_default(200, "relation_name")?;
        let plain: Vec<String> = deserializer.read_property(201, "exclude_list")?;
        let replace_list = deserializer.read_property_with_default(202, "replace_list")?;
        let columns = deserializer.read_property_with_default(203, "columns")?;
        let expr = deserializer.read_property_with_default(204, "expr")?;
        let unpacked = deserializer.read_property_with_explicit_default(205, "unpacked", false)?;
        let qualified: Vec<QualifiedColumnName> =
            deserializer.read_property_with_default(206, "qualified_exclude_list")?;
        let rename_list = deserializer.read_property_with_default(207, "rename_list")?;
        let mut exclude_list: Vec<QualifiedColumnName> =
            plain.into_iter().map(QualifiedColumnName::column).collect();
        exclude_list.extend(qualified);
        let star = Self {
            relation_name,
            exclude_list,
            replace_list,
            rename_list,
            columns,
            expr,
        };
        Ok((star, unpacked))
    }

    /// The deserialization arm of `ParsedExpression` for `STAR`: legacy "unpacked" stars
    /// become `OPERATOR_UNPACK(star)` (DuckDB's `DeserializeStarExpression`).
    pub(crate) fn deserialize_kind(
        deserializer: &mut dyn Deserializer,
        expression_type: &mut ExpressionType,
    ) -> Result<ParsedExpressionKind> {
        let (star, unpacked) = Self::deserialize_with_unpacked(deserializer)?;
        if !unpacked {
            return Ok(ParsedExpressionKind::Star(Box::new(star)));
        }
        *expression_type = ExpressionType::OperatorUnpack;
        let child = ParsedExpression::new(ExpressionType::Star, ParsedExpressionKind::Star(Box::new(star)));
        Ok(ParsedExpressionKind::Operator(OperatorExpression {
            children: vec![child],
        }))
    }
}

impl DeserializeObject for StarExpression {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        Self::deserialize_with_unpacked(deserializer).map(|(star, _)| star)
    }
}
