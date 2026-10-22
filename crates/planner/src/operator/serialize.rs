//! Serialization of logical plans (DuckDB's `LogicalOperator::Serialize`), through the
//! property structs generated from `logical_operator.json`, as `json_serialize_plan`
//! prints them.

use std::sync::Arc;

use rustdb_ast::{CatalogType, CreateInfo, DropInfo, OnConflictAction, OnEntryNotFound, ParseInfo};
use rustdb_catalog::{CatalogEntry, EntryKind};
use rustdb_common::{Error, PhysicalIndex, Result};
use rustdb_function::named_parameters_in_map_order;
use rustdb_serde::{SerializeObject, Serializer};

use super::{
    BoundLimitNode, BoundMergeIntoAction, BoundPivotInfo, CopyFunctionReturnType, CopyOverwriteMode,
    JoinCondition, LogicalComparisonJoin, LogicalCopyToFile, LogicalGet, LogicalKind, LogicalOperator,
};
use crate::function_serialization::{FunctionDataWriter, serialize_table_function};
use crate::generated::{logical_operator as properties, nodes};
use crate::{LimitNodeType, LogicalOperatorType};

impl LogicalOperator {
    /// Whether DuckDB can serialize the operator (`SupportSerialization`): EXPLAIN,
    /// PREPARE, EXECUTE, PRAGMA and CREATE SECRET cannot.
    pub fn supports_serialization(&self) -> bool {
        !matches!(
            self.kind,
            LogicalKind::Explain { .. }
                | LogicalKind::Prepare { .. }
                | LogicalKind::Execute { .. }
                | LogicalKind::Pragma { .. }
                | LogicalKind::CreateSecret(_)
        )
    }

    /// The first operator of the plan, children first, that cannot be serialized
    /// (`OperatorSupportsSerialization` of `json_serialize_plan`).
    pub fn unserializable_operator(&self) -> Option<LogicalOperatorType> {
        self.children
            .iter()
            .find_map(Self::unserializable_operator)
            .or_else(|| (!self.supports_serialization()).then(|| self.operator_type()))
    }
}

impl SerializeObject for LogicalOperator {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        let children = match &self.kind {
            LogicalKind::CreateIndex(create) => std::slice::from_ref(&*create.scan),
            _ => &self.children,
        };
        properties::LogicalOperator {
            operator_type: self.operator_type(),
            children,
        }
        .serialize_fields(serializer)?;
        let expressions = &self.expressions;
        match &self.kind {
            LogicalKind::Projection { table_index } => properties::LogicalProjection {
                table_index: *table_index,
                expressions,
            }
            .serialize_fields(serializer),
            LogicalKind::Filter { projection_map } => properties::LogicalFilter {
                expressions,
                projection_map,
            }
            .serialize_fields(serializer),
            LogicalKind::Aggregate(aggregate) => properties::LogicalAggregate {
                expressions,
                group_index: aggregate.group_index,
                aggregate_index: aggregate.aggregate_index,
                groupings_index: aggregate.groupings_index,
                groups: &aggregate.groups,
                grouping_sets: &aggregate.grouping_sets,
                grouping_functions: &aggregate.grouping_functions,
                distinct_validity: aggregate.distinct_validity,
            }
            .serialize_fields(serializer),
            LogicalKind::Window { window_index } => properties::LogicalWindow {
                window_index: *window_index,
                expressions,
            }
            .serialize_fields(serializer),
            LogicalKind::Unnest { unnest_index } => properties::LogicalUnnest {
                unnest_index: *unnest_index,
                expressions,
            }
            .serialize_fields(serializer),
            LogicalKind::Limit { limit, offset } => properties::LogicalLimit {
                limit_val: limit,
                offset_val: offset,
            }
            .serialize_fields(serializer),
            LogicalKind::Order {
                orders,
                projection_map,
            } => properties::LogicalOrder {
                orders,
                projections: projection_map,
            }
            .serialize_fields(serializer),
            LogicalKind::TopN {
                orders,
                limit,
                offset,
                ..
            } => properties::LogicalTopN {
                orders,
                limit: *limit,
                offset: *offset,
            }
            .serialize_fields(serializer),
            LogicalKind::Distinct {
                distinct_type,
                distinct_targets,
                order_by,
            } => properties::LogicalDistinct {
                distinct_type: *distinct_type,
                distinct_targets,
                order_by: order_by.as_ref(),
            }
            .serialize_fields(serializer),
            LogicalKind::Sample { sample_options } => {
                properties::LogicalSample { sample_options }.serialize_fields(serializer)
            }
            LogicalKind::Pivot {
                pivot_index,
                bound_pivot,
            } => properties::LogicalPivot {
                pivot_index: *pivot_index,
                bound_pivot,
            }
            .serialize_fields(serializer),
            LogicalKind::Get(get) => serialize_get(get, serializer),
            LogicalKind::ChunkGet {
                table_index,
                chunk_types,
                collection,
            } => {
                let collection = rustdb_types::ColumnDataCollection::from_shared(
                    chunk_types.clone(),
                    Arc::clone(collection),
                );
                properties::LogicalColumnDataGet {
                    table_index: *table_index,
                    chunk_types,
                    collection: Some(&collection),
                }
                .serialize_fields(serializer)
            }
            LogicalKind::DelimGet {
                table_index,
                chunk_types,
            } => properties::LogicalDelimGet {
                table_index: *table_index,
                chunk_types,
            }
            .serialize_fields(serializer),
            LogicalKind::ExpressionGet {
                table_index,
                expr_types,
                expressions,
            } => properties::LogicalExpressionGet {
                table_index: *table_index,
                expr_types,
                expressions,
            }
            .serialize_fields(serializer),
            LogicalKind::DummyScan { table_index } => properties::LogicalDummyScan {
                table_index: *table_index,
            }
            .serialize_fields(serializer),
            LogicalKind::EmptyResult {
                return_types,
                bindings,
            } => properties::LogicalEmptyResult {
                return_types,
                bindings,
            }
            .serialize_fields(serializer),
            LogicalKind::CteRef {
                table_index,
                cte_index,
                chunk_types,
                bound_columns,
                is_recurring,
                ..
            } => properties::LogicalCteRef {
                table_index: *table_index,
                cte_index: *cte_index,
                chunk_types,
                bound_columns,
                is_recurring: *is_recurring,
            }
            .serialize_fields(serializer),
            LogicalKind::ComparisonJoin(join) => serialize_comparison_join(join, serializer),
            LogicalKind::DependentJoin(dependent) => serialize_comparison_join(&dependent.join, serializer),
            LogicalKind::AnyJoin { join_type, condition } => properties::LogicalAnyJoin {
                join_type: *join_type,
                mark_index: 0,
                left_projection_map: &[],
                right_projection_map: &[],
                condition,
            }
            .serialize_fields(serializer),
            LogicalKind::CrossProduct | LogicalKind::PositionalJoin => Ok(()),
            LogicalKind::SetOperation {
                table_index,
                column_count,
                setop_all,
                allow_out_of_order,
                ..
            } => properties::LogicalSetOperation {
                table_index: *table_index,
                column_count: *column_count,
                setop_all: *setop_all,
                allow_out_of_order: *allow_out_of_order,
            }
            .serialize_fields(serializer),
            LogicalKind::RecursiveCte {
                ctename,
                table_index,
                column_count,
                union_all,
                key_targets,
                ref_recurring,
                ..
            } => properties::LogicalRecursiveCte {
                union_all: *union_all,
                ctename,
                table_index: *table_index,
                column_count: *column_count,
                key_targets,
                ref_recurring: *ref_recurring,
            }
            .serialize_fields(serializer),
            LogicalKind::MaterializedCte {
                ctename,
                table_index,
                column_count,
                materialize,
                ..
            } => properties::LogicalMaterializedCte {
                table_index: *table_index,
                column_count: *column_count,
                ctename,
                materialize: *materialize,
            }
            .serialize_fields(serializer),
            LogicalKind::Set { name, value, scope } => properties::LogicalSet {
                name,
                value,
                scope: *scope,
            }
            .serialize_fields(serializer),
            LogicalKind::Reset { name, scope } => {
                properties::LogicalReset { name, scope: *scope }.serialize_fields(serializer)
            }
            LogicalKind::Attach(info) => serialize_simple(&ParseInfo::Attach(info.clone()), serializer),
            LogicalKind::Detach(info) => serialize_simple(&ParseInfo::Detach(info.clone()), serializer),
            LogicalKind::Load(info) => serialize_simple(&ParseInfo::Load(info.clone()), serializer),
            LogicalKind::AlterDatabase(info) => {
                serialize_simple(&ParseInfo::Alter(Box::new(info.clone())), serializer)
            }
            LogicalKind::Transaction { info } => {
                serialize_simple(&ParseInfo::Transaction(info.clone()), serializer)
            }
            LogicalKind::Deallocate { name } => {
                // `DEALLOCATE` is a `DROP` of the prepared statement (`Binder::Bind(DropStatement&)`).
                let info = DropInfo {
                    catalog_type: CatalogType::PreparedStatement,
                    catalog: String::new(),
                    schema: String::new(),
                    name: name.clone(),
                    if_not_found: OnEntryNotFound::ThrowException,
                    cascade: false,
                    allow_drop_internal: false,
                    extra_drop_info: None,
                };
                serialize_simple(&ParseInfo::Drop(Box::new(info)), serializer)
            }
            LogicalKind::Drop(info) => serialize_simple(&ParseInfo::Drop(info.clone()), serializer),
            LogicalKind::Alter(alter) => {
                let info = alter
                    .info
                    .as_deref()
                    .ok_or_else(|| Error::internal("ALTER without its statement"))?;
                serialize_simple(&ParseInfo::Alter(Box::new(info.clone())), serializer)
            }
            LogicalKind::CreateTable(create) => {
                properties::LogicalCreateTable { info: &create.info }.serialize_fields(serializer)
            }
            LogicalKind::Create { info, .. } => {
                properties::LogicalCreate { info }.serialize_fields(serializer)
            }
            LogicalKind::CreateIndex(create) => properties::LogicalCreateIndex {
                info: &create.info,
                unbound_expressions: &create.unbound_expressions,
                alter_table_info: None,
            }
            .serialize_fields(serializer),
            LogicalKind::Insert(insert) => {
                let table_info = table_info(&insert.catalog, &insert.schema, &insert.table)?;
                let column_index_map = index_map(&insert.column_index_map);
                properties::LogicalInsert {
                    table_info: &table_info,
                    insert_values: &[],
                    column_index_map: &column_index_map,
                    expected_types: &insert.expected_types,
                    table_index: insert.table_index,
                    return_chunk: insert.return_chunk,
                    bound_defaults: &insert.bound_defaults,
                    action_type: OnConflictAction::Throw,
                    expected_set_types: &[],
                    on_conflict_filter: &[],
                    on_conflict_condition: None,
                    do_update_condition: None,
                    set_columns: &[],
                    set_types: &[],
                    excluded_table_index: 0,
                    columns_to_fetch: &[],
                    source_columns: &[],
                    expressions,
                    update_is_del_and_insert: false,
                }
                .serialize_fields(serializer)
            }
            LogicalKind::Delete(delete) => {
                let table_info = table_info(&delete.catalog, &delete.schema, &delete.table)?;
                properties::LogicalDelete {
                    table_info: &table_info,
                    table_index: delete.table_index,
                    return_chunk: delete.return_chunk,
                    expressions,
                }
                .serialize_fields(serializer)
            }
            LogicalKind::Update(update) => {
                let table_info = table_info(&update.catalog, &update.schema, &update.table)?;
                let columns = physical_indexes(&update.columns);
                properties::LogicalUpdate {
                    table_info: &table_info,
                    table_index: update.table_index,
                    return_chunk: update.return_chunk,
                    expressions,
                    columns: &columns,
                    bound_defaults: &update.bound_defaults,
                    update_is_del_and_insert: update.update_is_del_and_insert,
                }
                .serialize_fields(serializer)
            }
            LogicalKind::MergeInto(merge) => {
                let table_info = table_info(&merge.catalog, &merge.schema, &merge.table)?;
                properties::LogicalMergeInto {
                    table_info: &table_info,
                    table_index: merge.table_index,
                    bound_defaults: &merge.bound_defaults,
                    row_id_start: merge.row_id_start as u64,
                    source_marker: merge.source_marker.map(|marker| marker as u64),
                    actions: &merge.actions,
                    return_chunk: merge.return_chunk,
                }
                .serialize_fields(serializer)
            }
            LogicalKind::Vacuum(vacuum) => {
                // `LogicalVacuum::Serialize`.
                serializer.write_property(200, "info", &Some(ParseInfo::Vacuum(vacuum.info.clone())))?;
                serializer.write_property(201, "column_id_map", &IndexMap(&vacuum.column_id_map))
            }
            LogicalKind::CopyToFile(copy) => serialize_copy_to_file(copy, serializer),
            LogicalKind::Export(_) => Err(Error::not_implemented(format!(
                "Serializing {} is not supported yet",
                self.operator_type().name()
            ))),
            LogicalKind::Explain { .. }
            | LogicalKind::Prepare { .. }
            | LogicalKind::Execute { .. }
            | LogicalKind::Pragma { .. }
            | LogicalKind::CreateSecret(_) => Err(Error::invalid_input(format!(
                "Operator '{}' does not support serialization",
                self.operator_type().name()
            ))),
        }
    }
}

rustdb_serde::impl_pointee_serialize!(LogicalOperator, BoundMergeIntoAction);

/// `LogicalSimple::Serialize`.
fn serialize_simple(info: &ParseInfo, serializer: &mut dyn Serializer) -> Result<()> {
    properties::LogicalSimple { info }.serialize_fields(serializer)
}

/// `LogicalComparisonJoin::Serialize`.
fn serialize_comparison_join(join: &LogicalComparisonJoin, serializer: &mut dyn Serializer) -> Result<()> {
    properties::LogicalComparisonJoin {
        join_type: join.join_type,
        mark_index: join.mark_index,
        left_projection_map: &join.left_projection_map,
        right_projection_map: &join.right_projection_map,
        conditions: &join.conditions,
        mark_types: &join.mark_types,
        duplicate_eliminated_columns: &join.duplicate_eliminated_columns,
        delim_flipped: join.delim_flipped,
        predicate: join.predicate.as_ref(),
    }
    .serialize_fields(serializer)
}

/// `LogicalGet::Serialize`.
fn serialize_get(get: &LogicalGet, serializer: &mut dyn Serializer) -> Result<()> {
    serializer.write_property(200, "table_index", &get.table_index)?;
    serializer.write_property(201, "returned_types", &get.returned_types)?;
    serializer.write_property(202, "names", &get.names)?;
    // 203 `column_ids` is no longer written.
    serializer.write_property(204, "projection_ids", &get.projection_ids)?;
    serializer.write_property(205, "table_filters", &get.table_filters)?;
    // A table scan's bind data (`TableScanBindData`) is the scanned table, which rustdb
    // keeps in the `LogicalGet`.
    let table_scan = get.table.as_ref().map(|table| {
        move |serializer: &mut dyn Serializer| {
            let (catalog, schema) = get.table_schema.clone().unwrap_or_default();
            serialize_table_scan_bind_data(serializer, &catalog, &schema, table.name(), get.is_create_index)
        }
    });
    serialize_table_function(
        serializer,
        &get.function,
        get.bind_data.as_deref(),
        table_scan.as_ref().map(|write| write as FunctionDataWriter<'_>),
    )?;
    if table_scan.is_none() && get.function.serialize.is_none() {
        // Without bind data the arguments are kept to bind the function again.
        serializer.write_property(206, "parameters", &get.parameters)?;
        let named: Vec<(&str, &rustdb_types::Value)> = named_parameters_in_map_order(&get.named_parameters)
            .map(|(name, value)| (name.as_str(), value))
            .collect();
        serializer.write_property(207, "named_parameters", &NamedParameters(&named))?;
        serializer.write_property(208, "input_table_types", &get.input_table_types)?;
        serializer.write_property(209, "input_table_names", &get.input_table_names)?;
    }
    serializer.write_property(210, "projected_input", &get.projected_input)?;
    serializer.write_property_with_default(211, "column_indexes", &get.column_ids)?;
    let extra_info = nodes::ExtraOperatorInfo {
        file_filters: &get.file_filters.filters,
        total_files: get.file_filters.total_files,
        filtered_files: get.file_filters.filtered_files,
        sample_options: get.sample_options.as_deref(),
    };
    let default_extra_info = extra_info.file_filters.is_empty()
        && extra_info.total_files.is_none()
        && extra_info.filtered_files.is_none()
        && extra_info.sample_options.is_none();
    serializer.write_optional_with(212, "extra_info", default_extra_info, |serializer| {
        serializer.write_object(&extra_info)
    })?;
    serializer.write_property_with_explicit_default(
        213,
        "ordinality_idx",
        &rustdb_serde::OptionalIdx(get.ordinality_index),
        &rustdb_serde::OptionalIdx(None),
    )?;
    serializer.write_property_with_default(214, "row_group_order_options", &get.row_group_order.as_ref())
}

/// `LogicalCopyToFile::Serialize`.
fn serialize_copy_to_file(copy: &LogicalCopyToFile, serializer: &mut dyn Serializer) -> Result<()> {
    use crate::{CopyFunctionReturnType as ReturnType, CopyOverwriteMode as Mode, PreserveOrderType};
    let overwrite_mode = match copy.overwrite_mode {
        CopyOverwriteMode::ErrorOnConflict => Mode::CopyErrorOnConflict,
        CopyOverwriteMode::Overwrite => Mode::CopyOverwrite,
        CopyOverwriteMode::OverwriteOrIgnore => Mode::CopyOverwriteOrIgnore,
        CopyOverwriteMode::Append => Mode::CopyAppend,
    };
    let return_type = match copy.return_type {
        CopyFunctionReturnType::ChangedRows => ReturnType::ChangedRows,
        CopyFunctionReturnType::ChangedRowsAndFileList => ReturnType::ChangedRowsAndFileList,
        CopyFunctionReturnType::WrittenFileStatistics => ReturnType::WrittenFileStatistics,
    };
    let preserve_order = match copy.preserve_order {
        None => PreserveOrderType::Automatic,
        Some(true) => PreserveOrderType::PreserveOrder,
        Some(false) => PreserveOrderType::DontPreserveOrder,
    };
    serializer.write_property(200, "file_path", &copy.file_path)?;
    serializer.write_property(201, "use_tmp_file", &copy.use_tmp_file)?;
    serializer.write_property(202, "filename_pattern", &copy.filename_pattern)?;
    serializer.write_property(203, "overwrite_or_ignore", &overwrite_mode)?;
    serializer.write_property(204, "per_thread_output", &copy.per_thread_output)?;
    serializer.write_property(205, "partition_output", &copy.partition_output)?;
    serializer.write_property(206, "partition_columns", &copy.partition_columns)?;
    serializer.write_property(207, "names", &copy.names)?;
    serializer.write_property(208, "expected_types", &copy.expected_types)?;
    serializer.write_property(209, "copy_info", &Some(ParseInfo::Copy(copy.copy_info.clone())))?;
    serializer.write_property(210, "function_name", &*copy.function.name)?;
    serializer.write_property(211, "function_has_serialize", &copy.function.serialize.is_some())?;
    if let Some(serialize) = &copy.function.serialize {
        serializer.write_object_property(212, "function_data", |serializer| {
            serialize(serializer, copy.bind_data.as_ref(), &copy.function)
        })?;
    }
    serializer.write_property(213, "file_extension", &copy.file_extension)?;
    serializer.write_property(214, "rotate", &copy.rotate)?;
    serializer.write_property(215, "return_type", &return_type)?;
    serializer.write_optional(
        216,
        "write_partition_columns",
        copy.write_partition_columns,
        &copy.write_partition_columns,
    )?;
    serializer.write_optional(
        217,
        "write_empty_file",
        copy.write_empty_file,
        &copy.write_empty_file,
    )?;
    serializer.write_optional(
        218,
        "preserve_order",
        preserve_order == PreserveOrderType::Automatic,
        &preserve_order,
    )?;
    serializer.write_optional(
        219,
        "hive_file_pattern",
        copy.hive_file_pattern,
        &copy.hive_file_pattern,
    )?;
    serializer.write_property_with_explicit_default(
        220,
        "file_size_bytes",
        &rustdb_serde::OptionalIdx(copy.file_size_bytes),
        &rustdb_serde::OptionalIdx(None),
    )
}

/// `TableScanSerialize`: the scanned table.
fn serialize_table_scan_bind_data(
    serializer: &mut dyn Serializer,
    catalog: &str,
    schema: &str,
    table: &str,
    is_create_index: bool,
) -> Result<()> {
    serializer.write_property(100, "catalog", catalog)?;
    serializer.write_property(101, "schema", schema)?;
    serializer.write_property(102, "table", table)?;
    serializer.write_property(103, "is_index_scan", &false)?;
    serializer.write_property(104, "is_create_index", &is_create_index)?;
    serializer.write_property_with_default(105, "result_ids", &Vec::<i64>::new())
}

/// A `named_parameter_map_t` (an unordered map) in its iteration order.
struct NamedParameters<'a>(&'a [(&'a str, &'a rustdb_types::Value)]);

impl rustdb_serde::Serialize for NamedParameters<'_> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.on_list_begin(self.0.len());
        for (name, value) in self.0 {
            serializer.on_object_begin();
            serializer.write_property(0, "key", *name)?;
            serializer.write_property(1, "value", *value)?;
            serializer.on_object_end();
        }
        serializer.on_list_end();
        Ok(())
    }
}

/// An `unordered_map<idx_t, idx_t>` from each position to its value, in libc++'s
/// iteration order (`std::hash<idx_t>` is the identity).
struct IndexMap<'a>(&'a [usize]);

impl rustdb_serde::Serialize for IndexMap<'_> {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        let mut map = rustdb_common::libcxx::UnorderedOrder::default();
        for key in 0..self.0.len() {
            map.insert(key as u64, key, |a, b| a == b);
        }
        let order = map.order();
        serializer.on_list_begin(order.len());
        for node in order {
            let key = map.keys()[node];
            serializer.on_object_begin();
            serializer.write_property(0, "key", &key)?;
            serializer.write_property(1, "value", &self.0[key])?;
            serializer.on_object_end();
        }
        serializer.on_list_end();
        Ok(())
    }
}

/// `TableCatalogEntry::GetInfo` of the modified table.
fn table_info(catalog: &str, schema: &str, entry: &CatalogEntry) -> Result<CreateInfo> {
    match &entry.kind {
        EntryKind::Table(table) => Ok(rustdb_catalog::checkpoint::table_create_info(
            catalog, schema, entry, table,
        )),
        _ => Err(Error::internal("modified entry is not a table")),
    }
}

/// An index map (`IndexVector<idx_t, PhysicalIndex>`) with `INVALID_INDEX` for the
/// columns without an input.
fn index_map(map: &[Option<usize>]) -> Vec<u64> {
    map.iter()
        .map(|index| index.map_or(u64::MAX, |index| index as u64))
        .collect()
}

fn physical_indexes(columns: &[usize]) -> Vec<PhysicalIndex> {
    columns
        .iter()
        .map(|&column| PhysicalIndex(column as u64))
        .collect()
}

impl rustdb_serde::Serialize for BoundLimitNode {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        // DuckDB's members default to 0 and -1.
        let (limit_type, constant_integer, constant_percentage, expression) = match self {
            Self::Unset => (LimitNodeType::Unset, 0, -1.0, None),
            Self::ConstantValue(value) => (LimitNodeType::ConstantValue, *value, -1.0, None),
            Self::ConstantPercentage(percentage) => (LimitNodeType::ConstantPercentage, 0, *percentage, None),
            Self::ExpressionValue(expression) => {
                (LimitNodeType::ExpressionValue, 0, -1.0, Some(&**expression))
            }
            Self::ExpressionPercentage(expression) => {
                (LimitNodeType::ExpressionPercentage, 0, -1.0, Some(&**expression))
            }
        };
        serializer.write_object(&nodes::BoundLimitNode {
            limit_type,
            constant_integer,
            constant_percentage,
            expression,
        })
    }
}

impl rustdb_serde::Serialize for JoinCondition {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_object(&nodes::JoinCondition {
            left: &self.left,
            right: &self.right,
            comparison: self.comparison,
        })
    }
}

impl rustdb_serde::Serialize for BoundPivotInfo {
    fn serialize(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_object(&nodes::BoundPivotInfo {
            group_count: self.group_count,
            types: &self.types,
            pivot_values: &self.pivot_values,
            aggregates: &self.aggregates,
        })
    }
}

impl SerializeObject for BoundMergeIntoAction {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        let columns = physical_indexes(&self.columns);
        let column_index_map = index_map(&self.column_index_map);
        properties::BoundMergeIntoAction {
            action_type: self.action_type,
            condition: self.condition.as_ref(),
            columns: &columns,
            expressions: &self.expressions,
            column_index_map: &column_index_map,
            update_is_del_and_insert: self.update_is_del_and_insert,
        }
        .serialize_fields(serializer)
    }
}
