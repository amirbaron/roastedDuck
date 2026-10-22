//! Serialization of bound expressions (DuckDB's `Expression::Serialize` and the
//! hand-written `Serialize` of function, aggregate and window expressions), through the
//! property structs generated from `expression.json`.

use std::sync::PoisonError;

use rustdb_common::{Error, Result};
use rustdb_serde::{SerializeObject, Serializer};

use super::{
    BoundAggregateExpression, BoundCaseCheck, BoundFunctionExpression, BoundOrderByNode, BoundOrderModifier,
    BoundParameterData, BoundWindowExpression, Expression, ExpressionKind,
};
use crate::function_serialization::{serialize_aggregate_function, serialize_scalar_function};
use crate::generated::{expression as properties, nodes, result_modifier};
use crate::{ColumnBinding, ExpressionType};

impl SerializeObject for Expression {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        properties::Expression {
            expression_class: self.expression_class(),
            expression_type: self.expression_type,
            alias: &self.alias,
            query_location: self.query_location,
        }
        .serialize_fields(serializer)?;
        let return_type = &self.return_type;
        match &self.kind {
            ExpressionKind::Aggregate(aggregate) => serialize_aggregate(self, aggregate, serializer),
            ExpressionKind::Between(between) => properties::BoundBetweenExpression {
                input: &between.input,
                lower: &between.lower,
                upper: &between.upper,
                lower_inclusive: between.lower_inclusive,
                upper_inclusive: between.upper_inclusive,
            }
            .serialize_fields(serializer),
            ExpressionKind::Case(case) => properties::BoundCaseExpression {
                return_type,
                case_checks: &case.case_checks,
                else_expr: &case.else_expr,
            }
            .serialize_fields(serializer),
            ExpressionKind::Cast(cast) => properties::BoundCastExpression {
                child: &cast.child,
                return_type,
                try_cast: cast.try_cast,
            }
            .serialize_fields(serializer),
            ExpressionKind::ColumnRef(column) => properties::BoundColumnRefExpression {
                return_type,
                binding: &column.binding,
                depth: column.depth,
            }
            .serialize_fields(serializer),
            ExpressionKind::Comparison(comparison) => properties::BoundComparisonExpression {
                left: &comparison.left,
                right: &comparison.right,
            }
            .serialize_fields(serializer),
            ExpressionKind::Conjunction(conjunction) => properties::BoundConjunctionExpression {
                children: &conjunction.children,
            }
            .serialize_fields(serializer),
            ExpressionKind::Constant(constant) => properties::BoundConstantExpression {
                value: &constant.value,
            }
            .serialize_fields(serializer),
            ExpressionKind::Default => {
                properties::BoundDefaultExpression { return_type }.serialize_fields(serializer)
            }
            ExpressionKind::Function(function) => serialize_function(self, function, serializer),
            ExpressionKind::Lambda(lambda) => properties::BoundLambdaExpression {
                return_type,
                lambda_expr: &lambda.lambda_expr,
                captures: &lambda.captures,
                parameter_count: lambda.parameter_count,
            }
            .serialize_fields(serializer),
            ExpressionKind::LambdaRef(lambda_ref) => properties::BoundLambdaRefExpression {
                return_type,
                binding: &lambda_ref.binding,
                lambda_index: lambda_ref.lambda_index,
                depth: lambda_ref.depth,
            }
            .serialize_fields(serializer),
            ExpressionKind::Operator(operator) => properties::BoundOperatorExpression {
                return_type,
                children: &operator.children,
            }
            .serialize_fields(serializer),
            ExpressionKind::Parameter(parameter) => {
                let data = parameter
                    .parameter_data
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                properties::BoundParameterExpression {
                    identifier: &parameter.identifier,
                    return_type,
                    parameter_data: &data,
                }
                .serialize_fields(serializer)
            }
            ExpressionKind::Reference(reference) => properties::BoundReferenceExpression {
                return_type,
                index: reference.index,
            }
            .serialize_fields(serializer),
            ExpressionKind::Unnest(unnest) => properties::BoundUnnestExpression {
                return_type,
                child: &unnest.child,
            }
            .serialize_fields(serializer),
            ExpressionKind::Window(window) => serialize_window(self, window, serializer),
            // Planning replaces these by operators; DuckDB has no serialization for them.
            ExpressionKind::Subquery(_) | ExpressionKind::Expanded(_) => Err(Error::serialization(format!(
                "Unsupported type for serialization of {}",
                self.expression_class().name()
            ))),
        }
    }
}

rustdb_serde::impl_pointee_serialize!(Expression, BoundOrderModifier, BoundParameterData);

/// `BoundFunctionExpression::Serialize`.
fn serialize_function(
    expression: &Expression,
    function: &BoundFunctionExpression,
    serializer: &mut dyn Serializer,
) -> Result<()> {
    serializer.write_property(200, "return_type", &expression.return_type)?;
    serializer.write_property(201, "children", &function.children)?;
    serialize_scalar_function(serializer, &function.function, function.bind_info.as_deref())?;
    serializer.write_property(202, "is_operator", &function.is_operator)
}

/// `BoundAggregateExpression::Serialize`.
fn serialize_aggregate(
    expression: &Expression,
    aggregate: &BoundAggregateExpression,
    serializer: &mut dyn Serializer,
) -> Result<()> {
    serializer.write_property(200, "return_type", &expression.return_type)?;
    serializer.write_property(201, "children", &aggregate.children)?;
    serialize_aggregate_function(serializer, &aggregate.function, aggregate.bind_info.as_deref())?;
    serializer.write_property(203, "aggregate_type", &aggregate.aggr_type)?;
    serializer.write_property_with_default(204, "filter", &aggregate.filter.as_ref())?;
    serializer.write_property_with_default(205, "order_bys", &aggregate.order_bys.as_ref())
}

/// `BoundWindowExpression::Serialize`.
fn serialize_window(
    expression: &Expression,
    window: &BoundWindowExpression,
    serializer: &mut dyn Serializer,
) -> Result<()> {
    serializer.write_property(200, "return_type", &expression.return_type)?;
    serializer.write_property(201, "children", &window.children)?;
    if expression.expression_type == ExpressionType::WindowAggregate {
        let aggregate = window
            .aggregate
            .as_ref()
            .ok_or_else(|| Error::internal("window aggregate without its aggregate"))?;
        serialize_aggregate_function(serializer, aggregate, window.bind_info.as_deref())?;
    }
    serializer.write_property(202, "partitions", &window.partitions)?;
    serializer.write_property(203, "orders", &window.orders)?;
    serializer.write_property_with_default(204, "filters", &window.filter_expr.as_ref())?;
    serializer.write_property(205, "ignore_nulls", &window.ignore_nulls)?;
    serializer.write_property(206, "start", &window.start)?;
    serializer.write_property(207, "end", &window.end)?;
    serializer.write_property_with_default(208, "start_expr", &window.start_expr.as_ref())?;
    serializer.write_property_with_default(209, "end_expr", &window.end_expr.as_ref())?;
    serializer.write_property_with_default(210, "offset_expr", &window.offset_expr.as_ref())?;
    serializer.write_property_with_default(211, "default_expr", &window.default_expr.as_ref())?;
    serializer.write_property(212, "exclude_clause", &window.exclude_clause)?;
    serializer.write_property(213, "distinct", &window.distinct)?;
    serializer.write_property(214, "arg_orders", &window.arg_orders)
}

impl rustdb_serde::Serialize for BoundCaseCheck {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_object(&nodes::BoundCaseCheck {
            when_expr: &self.when_expr,
            then_expr: &self.then_expr,
        })
    }
}

impl rustdb_serde::Serialize for BoundOrderByNode {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_object(&nodes::BoundOrderByNode {
            order_type: self.order_type,
            null_order: self.null_order,
            expression: &self.expression,
        })
    }
}

impl SerializeObject for BoundOrderModifier {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        result_modifier::BoundOrderModifier { orders: &self.orders }.serialize_fields(serializer)
    }
}

impl SerializeObject for BoundParameterData {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        nodes::BoundParameterData {
            value: &self.value,
            return_type: &self.return_type,
        }
        .serialize_fields(serializer)
    }
}

impl rustdb_serde::Serialize for ColumnBinding {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_object(&nodes::ColumnBinding {
            table_index: self.table_index,
            column_index: self.column_index,
        })
    }
}
