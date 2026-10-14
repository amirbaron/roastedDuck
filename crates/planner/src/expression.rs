use std::fmt;
use std::sync::Arc;

use rustdb_function::{FunctionData, ScalarFunction};
use rustdb_types::{LogicalType, Value};

/// Identifies a column produced by a logical operator: `(table index, column index)`
/// (DuckDB's `ColumnBinding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnBinding {
    pub table_index: u64,
    pub column_index: u64,
}

impl fmt::Display for ColumnBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#[{}.{}]", self.table_index, self.column_index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComparisonOp {
    Equal,
    NotEqual,
    LessThan,
    GreaterThan,
    LessThanOrEqual,
    GreaterThanOrEqual,
    IsDistinctFrom,
    IsNotDistinctFrom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConjunctionOp {
    And,
    Or,
}

/// Built-in operators that are not ordinary functions (DuckDB's `BoundOperatorExpression`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperatorOp {
    Not,
    IsNull,
    IsNotNull,
    In,
    NotIn,
    Coalesce,
    TryExpression,
}

/// A scalar function bound to concrete argument types.
#[derive(Clone, Debug)]
pub struct BoundFunction {
    pub function: ScalarFunction,
    pub children: Vec<Expression>,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// Whether this came from an operator (`a + b`) rather than a call (`add(a, b)`).
    pub is_operator: bool,
}

/// A cast between two types.
#[derive(Clone, Debug)]
pub struct BoundCast {
    pub child: Box<Expression>,
    pub target: LogicalType,
    pub try_cast: bool,
}

/// A bound, typed expression.
#[derive(Clone, Debug)]
pub enum Expression {
    Constant(Value),
    /// Column `index` of the input chunk (after column bindings are resolved).
    Reference {
        index: usize,
        ty: LogicalType,
    },
    /// A column of some operator's output, before resolution to a [`Expression::Reference`].
    ColumnRef {
        binding: ColumnBinding,
        ty: LogicalType,
        depth: usize,
    },
    Function(BoundFunction),
    Cast(BoundCast),
    Comparison {
        op: ComparisonOp,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Conjunction {
        op: ConjunctionOp,
        children: Vec<Expression>,
    },
    Operator {
        op: OperatorOp,
        children: Vec<Expression>,
        ty: LogicalType,
    },
    Case {
        checks: Vec<(Expression, Expression)>,
        otherwise: Box<Expression>,
        ty: LogicalType,
    },
    Between {
        input: Box<Expression>,
        lower: Box<Expression>,
        upper: Box<Expression>,
        lower_inclusive: bool,
        upper_inclusive: bool,
    },
}

impl Expression {
    /// The type this expression evaluates to.
    pub fn return_type(&self) -> LogicalType {
        match self {
            Self::Constant(value) => value.logical_type().clone(),
            Self::Reference { ty, .. }
            | Self::ColumnRef { ty, .. }
            | Self::Operator { ty, .. }
            | Self::Case { ty, .. } => ty.clone(),
            Self::Function(function) => function.function.return_type.clone(),
            Self::Cast(cast) => cast.target.clone(),
            Self::Comparison { .. } | Self::Conjunction { .. } | Self::Between { .. } => LogicalType::BOOLEAN,
        }
    }

    /// Whether the expression can be evaluated without any input row.
    pub fn is_foldable(&self) -> bool {
        match self {
            Self::Constant(_) => true,
            Self::Reference { .. } | Self::ColumnRef { .. } => false,
            Self::Function(function) => {
                function.function.stability == rustdb_function::Stability::Consistent
                    && function.children.iter().all(Self::is_foldable)
            }
            _ => self.children().all(Self::is_foldable),
        }
    }

    /// Direct child expressions.
    pub fn children(&self) -> Box<dyn Iterator<Item = &Expression> + '_> {
        match self {
            Self::Constant(_) | Self::Reference { .. } | Self::ColumnRef { .. } => {
                Box::new(std::iter::empty())
            }
            Self::Function(function) => Box::new(function.children.iter()),
            Self::Cast(cast) => Box::new(std::iter::once(cast.child.as_ref())),
            Self::Comparison { left, right, .. } => Box::new([left.as_ref(), right.as_ref()].into_iter()),
            Self::Conjunction { children, .. } | Self::Operator { children, .. } => Box::new(children.iter()),
            Self::Case {
                checks, otherwise, ..
            } => Box::new(
                checks
                    .iter()
                    .flat_map(|(when, then)| [when, then])
                    .chain(std::iter::once(otherwise.as_ref())),
            ),
            Self::Between {
                input, lower, upper, ..
            } => Box::new([input.as_ref(), lower.as_ref(), upper.as_ref()].into_iter()),
        }
    }
}
