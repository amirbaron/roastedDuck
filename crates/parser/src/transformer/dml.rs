//! DML statements: INSERT, UPDATE, DELETE, MERGE INTO and COPY (`transform_insert.cpp`,
//! `transform_upsert.cpp`, `transform_update.cpp`, `transform_delete.cpp`,
//! `transform_merge_into.cpp`, `transform_copy.cpp`).

use std::collections::BTreeMap;

use rustdb_ast::{
    CommonTableExpressionMap, CopyInfo, DeleteStatement, InsertColumnOrder, InsertStatement,
    MergeActionCondition, MergeActionType, MergeIntoAction, MergeIntoStatement, OnConflictAction,
    OnConflictInfo, ParsedExpression, ParsedExpressionKind, SqlStatementKind, TableRefKind, UpdateSetInfo,
    UpdateStatement,
};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_pg_parser::nodes::{
    CopyStmt, DefElem, DeleteStmt, IndexElem, InsertColumnOrder as PgInsertColumnOrder, InsertStmt, List,
    MatchAction, MergeAction, MergeActionType as PgMergeActionType, MergeIntoStmt, Node,
    OnConflictAction as PgOnConflictAction, OnConflictActionAlias, OnConflictClause, ResTarget, UpdateStmt,
};
use rustdb_types::LogicalTypeId;

use super::helpers::transform_qualified_name;
use super::{Transformer, cast_node, required, res_target_name, string_list};
use crate::hash_order::CiHashMap;

/// `TransformColumnOrder`.
const fn transform_column_order(order: PgInsertColumnOrder) -> InsertColumnOrder {
    match order {
        PgInsertColumnOrder::ByPosition => InsertColumnOrder::InsertByPosition,
        PgInsertColumnOrder::ByName => InsertColumnOrder::InsertByName,
    }
}

/// `TransformInsertColumns`: the names of the target columns.
fn transform_insert_columns(columns: &List) -> Result<Vec<rustdb_common::ByteString>> {
    columns
        .iter()
        .map(|node| Ok(res_target_name(node)?.unwrap_or_default()))
        .collect()
}

/// `TransformConflictTarget`: the columns of `ON CONFLICT (...)`.
fn transform_conflict_target(list: &List) -> Result<Vec<rustdb_common::ByteString>> {
    let mut columns = Vec::with_capacity(list.len());
    for node in list {
        let element: &IndexElem = cast_node(node)?;
        if !element.collation.is_empty() {
            return Err(Error::not_implemented("Index with collation not supported yet!"));
        }
        if !element.opclass.is_empty() {
            return Err(Error::not_implemented("Index with opclass not supported yet!"));
        }
        let Some(name) = &element.name else {
            return Err(Error::not_implemented(
                "Non-column index element not supported yet!",
            ));
        };
        if element.nulls_ordering as i32 != 0 {
            return Err(Error::not_implemented(
                "Index with null_ordering not supported yet!",
            ));
        }
        if element.ordering as i32 != 0 {
            return Err(Error::not_implemented("Index with ordering not supported yet!"));
        }
        columns.push(name.clone());
    }
    Ok(columns)
}

/// `DummyOnConflictClause`: `INSERT OR REPLACE` / `INSERT OR IGNORE`.
fn dummy_on_conflict_clause(alias: OnConflictActionAlias) -> Result<OnConflictInfo> {
    let action_type = match alias {
        OnConflictActionAlias::Replace => OnConflictAction::Replace,
        OnConflictActionAlias::Ignore => OnConflictAction::Nothing,
        OnConflictActionAlias::None => {
            return Err(Error::internal(
                "Type not implemented for PGOnConflictActionAlias",
            ));
        }
    };
    Ok(OnConflictInfo {
        action_type,
        indexed_columns: Vec::new(),
        set_info: None,
        condition: None,
    })
}

/// `CopyInfo()`: the defaults of a COPY / EXPORT.
pub(crate) fn copy_info() -> CopyInfo {
    CopyInfo {
        catalog: String::new().into(),
        schema: "main".to_owned().into(),
        table: String::new().into(),
        select_list: Vec::new(),
        is_from: false,
        format: rustdb_common::ByteString::new(),
        file_path: rustdb_common::ByteString::new(),
        options: CaseInsensitiveMap::default(),
        select_statement: None,
        is_format_auto_detected: true,
        parsed_options: CaseInsensitiveMap::default(),
        file_path_expression: None,
    }
}

impl<'a> Transformer<'a> {
    /// `TransformUpdateSetInfo`: `SET col = expr, ... [WHERE ...]`.
    fn transform_update_set_info(
        &mut self,
        targets: &'a List,
        where_clause: Option<&'a Node>,
    ) -> Result<UpdateSetInfo> {
        let mut info = UpdateSetInfo {
            condition: None,
            columns: Vec::with_capacity(targets.len()),
            expressions: Vec::with_capacity(targets.len()),
        };
        for node in targets {
            let target: &'a ResTarget = cast_node(node)?;
            if !target.indirection.is_empty() {
                return Err(Error::parser(
                    "Qualified column names in UPDATE .. SET not supported",
                ));
            }
            info.columns.push(target.name.clone().unwrap_or_default());
            info.expressions
                .push(self.transform_required(target.val.as_ref(), "SET value")?);
        }
        info.condition = self.transform_opt_boxed(where_clause)?;
        Ok(info)
    }

    /// `TransformOnConflictClause`.
    fn transform_on_conflict_clause(&mut self, clause: &'a OnConflictClause) -> Result<OnConflictInfo> {
        let action_type = match clause.action {
            PgOnConflictAction::None => OnConflictAction::Throw,
            PgOnConflictAction::Nothing => OnConflictAction::Nothing,
            PgOnConflictAction::Update => OnConflictAction::Update,
        };
        let mut info = OnConflictInfo {
            action_type,
            indexed_columns: Vec::new(),
            set_info: None,
            condition: None,
        };
        if let Some(infer) = &clause.infer {
            if infer.index_elems.is_empty() {
                return Err(Error::not_implemented(
                    "ON CONSTRAINT conflict target is not supported yet",
                ));
            }
            info.indexed_columns = transform_conflict_target(&infer.index_elems)?;
            info.condition = self.transform_opt_boxed(infer.where_clause.as_ref())?;
        }
        if info.action_type == OnConflictAction::Update {
            info.set_info = Some(Box::new(
                self.transform_update_set_info(&clause.target_list, clause.where_clause.as_ref())?,
            ));
        }
        Ok(info)
    }

    /// `TransformInsert`.
    pub(crate) fn transform_insert(&mut self, stmt: &'a InsertStmt) -> Result<SqlStatementKind> {
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = &stmt.with_clause {
            self.transform_cte(with, &mut cte_map)?;
        }
        let columns = transform_insert_columns(&stmt.cols)?;
        let returning_list = self.transform_expressions(&stmt.returning_list)?;
        let (select_statement, default_values) = match &stmt.select_stmt {
            Some(select) => (
                Some(Box::new(self.transform_select_stmt_node(select, false)?)),
                false,
            ),
            None => (None, true),
        };
        let relation = required(stmt.relation.as_deref(), "table")?;
        let qname = transform_qualified_name(relation)?;
        let mut on_conflict_info = None;
        let mut table_ref = None;
        if let Some(clause) = &stmt.on_conflict_clause {
            if stmt.on_conflict_alias != OnConflictActionAlias::None {
                return Err(Error::parser(
                    "You can not provide both OR REPLACE|IGNORE and an ON CONFLICT clause, please remove the first \
                     if you want to have more granual control",
                ));
            }
            on_conflict_info = Some(Box::new(self.transform_on_conflict_clause(clause)?));
            table_ref = Some(Box::new(self.transform_range_var(relation)?));
        }
        if stmt.on_conflict_alias != OnConflictActionAlias::None {
            on_conflict_info = Some(Box::new(dummy_on_conflict_clause(stmt.on_conflict_alias)?));
            table_ref = Some(Box::new(self.transform_range_var(relation)?));
        }
        Ok(SqlStatementKind::Insert(Box::new(InsertStatement {
            select_statement,
            columns,
            catalog: qname.catalog,
            schema: qname.schema,
            table: qname.name,
            table_ref,
            returning_list,
            on_conflict_info,
            cte_map,
            default_values,
            column_order: transform_column_order(stmt.insert_column_order),
        })))
    }

    /// `TransformUpdate`.
    pub(crate) fn transform_update(&mut self, stmt: &'a UpdateStmt) -> Result<SqlStatementKind> {
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = &stmt.with_clause {
            self.transform_cte(with, &mut cte_map)?;
        }
        let table = self.transform_range_var(required(stmt.relation.as_deref(), "table")?)?;
        let from_table = if stmt.from_clause.is_empty() {
            None
        } else {
            Some(Box::new(self.transform_from(&stmt.from_clause)?))
        };
        let set_info = self.transform_update_set_info(&stmt.target_list, stmt.where_clause.as_ref())?;
        let returning_list = self.transform_expressions(&stmt.returning_list)?;
        Ok(SqlStatementKind::Update(Box::new(UpdateStatement {
            table: Box::new(table),
            from_table,
            set_info: Box::new(set_info),
            returning_list,
            cte_map,
            prioritize_table_when_binding: false,
        })))
    }

    /// `TransformDelete`.
    pub(crate) fn transform_delete(&mut self, stmt: &'a DeleteStmt) -> Result<SqlStatementKind> {
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = &stmt.with_clause {
            self.transform_cte(with, &mut cte_map)?;
        }
        let condition = self.transform_opt_boxed(stmt.where_clause.as_ref())?;
        let table = self.transform_range_var(required(stmt.relation.as_deref(), "table")?)?;
        if !matches!(table.kind, TableRefKind::BaseTable(_)) {
            return Err(Error::invalid_input("Can only delete from base tables!"));
        }
        let mut using_clauses = Vec::with_capacity(stmt.using_clause.len());
        for node in &stmt.using_clause {
            using_clauses.push(self.transform_table_ref_node(node)?);
        }
        let returning_list = self.transform_expressions(&stmt.returning_list)?;
        Ok(SqlStatementKind::Delete(Box::new(DeleteStatement {
            table: Box::new(table),
            condition,
            using_clauses,
            returning_list,
            cte_map,
        })))
    }

    /// `TransformMergeIntoAction`.
    fn transform_merge_into_action(&mut self, action: &'a MatchAction) -> Result<MergeIntoAction> {
        let mut result = MergeIntoAction {
            action_type: MergeActionType::MergeUpdate,
            condition: self.transform_opt_boxed(action.and_clause.as_ref())?,
            update_info: None,
            insert_columns: Vec::new(),
            expressions: Vec::new(),
            column_order: InsertColumnOrder::InsertByPosition,
            default_values: false,
            exclude_columns: std::collections::BTreeSet::new(),
        };
        match action.action_type {
            PgMergeActionType::Update => {
                result.action_type = MergeActionType::MergeUpdate;
                if !action.update_targets.is_empty() {
                    result.update_info = Some(Box::new(
                        self.transform_update_set_info(&action.update_targets, None)?,
                    ));
                }
                result.column_order = transform_column_order(action.insert_column_order);
            }
            PgMergeActionType::Delete => result.action_type = MergeActionType::MergeDelete,
            PgMergeActionType::Insert => {
                result.action_type = MergeActionType::MergeInsert;
                result.insert_columns = transform_insert_columns(&action.insert_cols)?;
                self.transform_expression_list(&action.insert_values, &mut result.expressions)?;
                result.column_order = transform_column_order(action.insert_column_order);
                result.default_values = action.default_values;
            }
            PgMergeActionType::DoNothing => result.action_type = MergeActionType::MergeDoNothing,
            PgMergeActionType::Error => {
                result.action_type = MergeActionType::MergeError;
                if let Some(message) = &action.error_message {
                    result.expressions.push(self.transform_expression(message)?);
                }
            }
        }
        Ok(result)
    }

    /// `TransformMergeInto`.
    pub(crate) fn transform_merge_into(&mut self, stmt: &'a MergeIntoStmt) -> Result<SqlStatementKind> {
        let mut cte_map = CommonTableExpressionMap::default();
        if let Some(with) = &stmt.with_clause {
            self.transform_cte(with, &mut cte_map)?;
        }
        let target = self.transform_range_var(required(stmt.target_table.as_deref(), "target")?)?;
        let source = self.transform_table_ref_node(required(stmt.source.as_ref(), "source")?)?;
        let join_condition = self.transform_opt_boxed(stmt.join_condition.as_ref())?;
        let using_columns = string_list(&stmt.using_clause)?;
        let mut actions: BTreeMap<MergeActionCondition, Vec<MergeIntoAction>> = BTreeMap::new();
        let mut unconditional: BTreeMap<MergeActionCondition, MergeIntoAction> = BTreeMap::new();
        for node in &stmt.match_actions {
            let match_action: &'a MatchAction = cast_node(node)?;
            let action = self.transform_merge_into_action(match_action)?;
            let condition = match match_action.when {
                MergeAction::Matched => MergeActionCondition::WhenMatched,
                MergeAction::NotMatchedBySource => MergeActionCondition::WhenNotMatchedBySource,
                MergeAction::NotMatchedByTarget => MergeActionCondition::WhenNotMatchedByTarget,
            };
            if action.condition.is_none() {
                if unconditional.contains_key(&condition) {
                    let name = action_condition_to_string(condition);
                    return Err(Error::parser(rustdb_common::byte_format!(
                        "Unconditional {name} clause was already defined - only one unconditional {name} clause \
                         is supported"
                    )));
                }
                unconditional.insert(condition, action);
                continue;
            }
            actions.entry(condition).or_default().push(action);
        }
        for (condition, action) in unconditional {
            actions.entry(condition).or_default().push(action);
        }
        let returning_list = self.transform_expressions(&stmt.returning_list)?;
        Ok(SqlStatementKind::MergeInto(Box::new(MergeIntoStatement {
            target: Box::new(target),
            source: Box::new(source),
            join_condition,
            using_columns: using_columns.into_iter().collect(),
            actions,
            returning_list,
            cte_map,
        })))
    }

    /// `TransformCopyOptions`: `(FORMAT csv, HEADER, ...)`.
    pub(crate) fn transform_copy_options(&mut self, info: &mut CopyInfo, options: &'a List) -> Result<()> {
        let mut parsed: CiHashMap<Option<ParsedExpression>, rustdb_common::ByteString> =
            CiHashMap::new_with_keys();
        for node in options {
            let def_elem: &'a DefElem = cast_node(node)?;
            let name = def_elem.defname.clone().unwrap_or_default();
            if parsed.contains_key(&name) {
                return Err(Error::parser(rustdb_common::byte_format!(
                    "Unexpected duplicate option \"{name}\""
                )));
            }
            let expr = self.transform_opt_expression(def_elem.arg.as_ref())?;
            parsed.set(name, expr);
        }
        info.parsed_options = parsed.into_ordered().into_iter().collect();
        Ok(())
    }

    /// `TransformCopy`.
    pub(crate) fn transform_copy(&mut self, stmt: &'a CopyStmt) -> Result<SqlStatementKind> {
        let mut info = copy_info();
        info.is_from = stmt.is_from;
        let file_path = self.transform_required(stmt.filename.as_ref(), "file name")?;
        match &file_path.kind {
            ParsedExpressionKind::Constant(constant)
                if constant.value.logical_type().id() == LogicalTypeId::Varchar =>
            {
                info.file_path = constant.value.as_bytes().unwrap_or_default().into();
            }
            _ => info.file_path_expression = Some(Box::new(file_path)),
        }
        for node in &stmt.attlist {
            if let Some(name) = res_target_name(node)? {
                info.select_list.push(name);
            }
        }
        if let Some(relation) = &stmt.relation {
            let table_ref = self.transform_range_var(relation)?;
            let TableRefKind::BaseTable(table) = table_ref.kind else {
                return Err(Error::internal("COPY relation is not a base table"));
            };
            info.table = table.table_name;
            info.schema = table.schema_name;
            info.catalog = table.catalog_name;
        } else {
            let query = self.transform_select_node(required(stmt.query.as_ref(), "query")?, true)?;
            info.select_statement = Some(Box::new(query));
        }
        self.transform_copy_options(&mut info, &stmt.options)?;
        Ok(SqlStatementKind::Copy(Box::new(info)))
    }
}

/// `MergeIntoStatement::ActionConditionToString`.
const fn action_condition_to_string(condition: MergeActionCondition) -> &'static str {
    match condition {
        MergeActionCondition::WhenMatched => "WHEN MATCHED",
        MergeActionCondition::WhenNotMatchedBySource => "WHEN NOT MATCHED BY SOURCE",
        MergeActionCondition::WhenNotMatchedByTarget => "WHEN NOT MATCHED",
    }
}
