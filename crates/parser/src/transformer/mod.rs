//! The transformer: from the Postgres node tree to DuckDB's parse tree (DuckDB's
//! `Transformer`, `src/parser/transform/**`).
//!
//! Each `transform_*` method ports the C++ method of the same name. DuckDB creates child
//! transformers for subqueries and CTEs; here one transformer keeps a stack of
//! [`Frame`]s for the per-child state (named windows, CTEs seen so far) while the state
//! DuckDB keeps on the root transformer (parameters, pivot entries, nesting depth) lives
//! on the transformer itself.

pub mod build;
mod constant;
mod cte;
mod ddl;
mod dml;
pub(crate) mod expression;
pub(crate) mod function;
pub(crate) mod helpers;
mod misc;
mod node_tag;
mod operator;
mod pivot;
mod select;
pub(crate) mod star;
mod subquery;
mod tableref;
mod typename;

use roastedduck_ast::{
    CommonTableExpressionMap, ParsedExpression, SelectNode, SqlStatement, SqlStatementKind,
};
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result};
use roastedduck_pg_parser::nodes::{List, Node, RawStmt, Value, WindowDef};

use crate::ParserOptions;
use crate::hash_order::CiHashMap;

pub(crate) use node_tag::node_tag;

/// How a prepared-statement parameter was written (DuckDB's `PreparedParamType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamType {
    /// `?`
    AutoIncrement,
    /// `$1`
    Positional,
    /// `$name`
    Named,
    /// No parameter seen yet.
    Invalid,
}

/// A `CREATE TYPE ... AS ENUM (SELECT ...)` that a `PIVOT` without an `IN` list needs
/// (DuckDB's `CreatePivotEntry`).
#[derive(Debug)]
pub(crate) struct PivotEntry {
    pub(crate) enum_name: ByteString,
    pub(crate) base: Option<SelectNode>,
    pub(crate) base_cte_map: CommonTableExpressionMap,
    pub(crate) column: ParsedExpression,
    pub(crate) subquery: Option<roastedduck_ast::QueryNode>,
    pub(crate) has_parameters: bool,
}

/// The state of one (child) transformer of DuckDB.
#[derive(Debug, Default)]
struct Frame<'a> {
    /// `WINDOW name AS (...)` definitions of the current SELECT.
    window_clauses: CaseInsensitiveMap<&'a WindowDef, ByteString>,
    /// The CTEs defined so far (`stored_cte_map`), kept only when the query can contain
    /// a PIVOT that needs them.
    stored_cte_maps: Vec<CommonTableExpressionMap>,
    /// Whether a window definition is being transformed.
    in_window_definition: bool,
}

/// DuckDB's `Transformer`.
#[derive(Debug)]
pub(crate) struct Transformer<'a> {
    options: ParserOptions,
    /// The highest parameter index used so far (`prepared_statement_parameter_index`).
    param_count: u64,
    /// Parameter names (and numbers) seen so far, with their indexes.
    named_params: CiHashMap<u64, ByteString>,
    last_param_type: ParamType,
    pivot_entries: Vec<PivotEntry>,
    /// The current nesting depth (`stack_depth`).
    depth: u64,
    /// The state of the root transformer.
    root: Frame<'a>,
    /// The states of the child transformers, innermost last.
    children: Vec<Frame<'a>>,
    /// The query text, to tell whether it mentions PIVOT at all: CTEs are only recorded
    /// for `ExtractCTEsRecursive` then.
    query: &'a [u8],
    /// Whether `query` mentions PIVOT, once known.
    may_pivot: Option<bool>,
}

impl<'a> Transformer<'a> {
    pub(crate) fn new(options: ParserOptions, query: &'a [u8]) -> Self {
        Self {
            options,
            param_count: 0,
            named_params: CiHashMap::new_with_keys(),
            last_param_type: ParamType::Invalid,
            pivot_entries: Vec::new(),
            depth: 0,
            root: Frame::default(),
            children: Vec::new(),
            query,
            may_pivot: None,
        }
    }

    /// `TransformParseTree`.
    pub(crate) fn transform_parse_tree(&mut self, tree: &'a [RawStmt]) -> Result<Vec<SqlStatement>> {
        self.depth = 0;
        let mut statements = Vec::with_capacity(tree.len());
        for raw in tree {
            self.clear();
            let mut statement = self.transform_raw_statement(raw)?;
            if !self.pivot_entries.is_empty() {
                statement = self.create_pivot_statement(statement)?;
            }
            statements.push(statement);
        }
        Ok(statements)
    }

    /// `Clear`: forgets parameters and pivot entries between statements.
    fn clear(&mut self) {
        self.clear_parameters();
        self.pivot_entries.clear();
    }

    /// `ClearParameters`.
    fn clear_parameters(&mut self) {
        self.param_count = 0;
        self.named_params.clear();
    }

    /// `TransformStatement` of a raw statement: the statement with its location.
    fn transform_raw_statement(&mut self, raw: &'a RawStmt) -> Result<SqlStatement> {
        let node = required(raw.stmt.as_ref(), "statement")?;
        let mut statement = self.transform_statement(node)?;
        statement.location = u64::try_from(raw.stmt_location).unwrap_or(0);
        statement.length = u64::try_from(raw.stmt_len).unwrap_or(0);
        Ok(statement)
    }

    /// `TransformStatement`: the statement, with the named-parameter map.
    pub(crate) fn transform_statement(&mut self, node: &'a Node) -> Result<SqlStatement> {
        if let Node::RawStmt(raw) = node {
            return self.transform_raw_statement(raw);
        }
        let mut statement = SqlStatement::new(self.transform_statement_internal(node)?);
        if !self.named_params.is_empty() {
            // A copy-assignment of the map, which keeps its iteration order.
            let map: CaseInsensitiveMap<u64, ByteString> = self
                .named_params
                .iter()
                .map(|(key, value)| (key, *value))
                .collect();
            if let SqlStatementKind::Select(select) = &mut statement.kind {
                select.named_param_map = map.clone();
            }
            statement.named_param_map = map;
        }
        Ok(statement)
    }

    /// `TransformStatementInternal`.
    fn transform_statement_internal(&mut self, node: &'a Node) -> Result<SqlStatementKind> {
        Ok(match node {
            Node::SelectStmt(stmt) => SqlStatementKind::Select(self.transform_select_stmt(stmt, true)?),
            Node::CreateStmt(stmt) => self.transform_create_table(stmt)?,
            Node::CreateSchemaStmt(stmt) => Self::transform_create_schema(stmt)?,
            Node::ViewStmt(stmt) => self.transform_create_view(stmt)?,
            Node::CreateSeqStmt(stmt) => Self::transform_create_sequence(stmt)?,
            Node::CreateFunctionStmt(stmt) => self.transform_create_function(stmt)?,
            Node::DropStmt(stmt) => Self::transform_drop(stmt)?,
            Node::InsertStmt(stmt) => self.transform_insert(stmt)?,
            Node::CopyStmt(stmt) => self.transform_copy(stmt)?,
            Node::TransactionStmt(stmt) => Self::transform_transaction(stmt)?,
            Node::DeleteStmt(stmt) => self.transform_delete(stmt)?,
            Node::UpdateStmt(stmt) => self.transform_update(stmt)?,
            Node::UpdateExtensionsStmt(stmt) => Self::transform_update_extensions(stmt)?,
            Node::IndexStmt(stmt) => self.transform_create_index(stmt)?,
            Node::AlterTableStmt(stmt) => self.transform_alter(stmt)?,
            Node::AlterDatabaseStmt(stmt) => Self::transform_alter_database(stmt)?,
            Node::RenameStmt(stmt) => Self::transform_rename(stmt)?,
            Node::PrepareStmt(stmt) => self.transform_prepare(stmt)?,
            Node::ExecuteStmt(stmt) => self.transform_execute(stmt)?,
            Node::DeallocateStmt(stmt) => Self::transform_deallocate(stmt)?,
            Node::CreateTableAsStmt(stmt) => self.transform_create_table_as(stmt)?,
            Node::PragmaStmt(stmt) => self.transform_pragma(stmt)?,
            Node::ExportStmt(stmt) => self.transform_export(stmt)?,
            Node::ImportStmt(stmt) => Self::transform_import(stmt)?,
            Node::ExplainStmt(stmt) => self.transform_explain(stmt)?,
            Node::VacuumStmt(stmt) => self.transform_vacuum(stmt)?,
            Node::VariableShowStmt(stmt) => SqlStatementKind::Select(self.transform_show_stmt(stmt)?),
            Node::VariableShowSelectStmt(stmt) => {
                SqlStatementKind::Select(self.transform_show_select_stmt(stmt)?)
            }
            Node::CallStmt(stmt) => self.transform_call(stmt)?,
            Node::VariableSetStmt(stmt) => self.transform_set(stmt)?,
            Node::CheckPointStmt(stmt) => Self::transform_checkpoint(stmt)?,
            Node::LoadStmt(stmt) => Self::transform_load(stmt),
            Node::CreateTypeStmt(stmt) => self.transform_create_type(stmt)?,
            Node::AlterSeqStmt(stmt) => Self::transform_alter_sequence(stmt)?,
            Node::AttachStmt(stmt) => self.transform_attach(stmt)?,
            Node::DetachStmt(stmt) => Self::transform_detach(stmt),
            Node::UseStmt(stmt) => Self::transform_use(stmt)?,
            Node::CopyDatabaseStmt(stmt) => Self::transform_copy_database(stmt)?,
            Node::CreateSecretStmt(stmt) => self.transform_secret(stmt)?,
            Node::DropSecretStmt(stmt) => Self::transform_drop_secret(stmt)?,
            Node::CommentOnStmt(stmt) => self.transform_comment_on(stmt)?,
            Node::MergeIntoStmt(stmt) => self.transform_merge_into(stmt)?,
            other => return Err(Error::not_implemented(node_tag(other).1)),
        })
    }

    // ------------------------------------------------------------------------------
    // Nesting depth
    // ------------------------------------------------------------------------------

    /// `StackCheck(extra)` without keeping the depth: fails when the depth would reach
    /// `max_expression_depth`.
    pub(crate) fn check_depth(&self, extra: u64) -> Result<()> {
        if self.depth + extra >= self.options.max_expression_depth {
            return Err(Error::parser(roastedduck_common::byte_format!(
                "Max expression depth limit of {} exceeded. Use \"SET max_expression_depth TO x\" to increase the \
                 maximum expression depth.",
                self.options.max_expression_depth
            )));
        }
        Ok(())
    }

    /// Runs `body` one level deeper (`auto stack_checker = StackCheck()`).
    pub(crate) fn nested<R>(&mut self, body: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        self.check_depth(1)?;
        self.depth += 1;
        let result = body(self);
        self.depth -= 1;
        result
    }

    // ------------------------------------------------------------------------------
    // Child transformers
    // ------------------------------------------------------------------------------

    /// Runs `body` in a child transformer (`Transformer child(*this)`).
    pub(crate) fn child<R>(&mut self, body: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        self.children.push(Frame::default());
        let result = body(self);
        self.children.pop();
        result
    }

    fn frame(&mut self) -> &mut Frame<'a> {
        self.children.last_mut().unwrap_or(&mut self.root)
    }

    /// The frames from the innermost transformer out to the root.
    fn frames_inside_out(&self) -> impl Iterator<Item = &Frame<'a>> {
        self.children.iter().rev().chain(std::iter::once(&self.root))
    }

    /// `InWindowDefinition`: whether this or an enclosing transformer is inside a
    /// window definition.
    pub(crate) fn in_window_definition(&self) -> bool {
        self.frames_inside_out().any(|frame| frame.in_window_definition)
    }

    // ------------------------------------------------------------------------------
    // Parameters
    // ------------------------------------------------------------------------------

    /// `ParamTypeCheck`: named and positional parameters cannot be mixed.
    fn param_type_check(&self, new_type: ParamType) -> Result<()> {
        let last = self.last_param_type;
        if last == ParamType::Invalid {
            return Ok(());
        }
        if (last == ParamType::Named) != (new_type == ParamType::Named) {
            return Err(Error::not_implemented(
                "Mixing named and positional parameters is not supported yet",
            ));
        }
        Ok(())
    }

    /// `SetParam`.
    pub(crate) fn set_param(&mut self, identifier: &[u8], index: u64, param_type: ParamType) -> Result<()> {
        self.param_type_check(param_type)?;
        self.last_param_type = param_type;
        self.named_params.set(ByteString::from(identifier), index);
        Ok(())
    }

    /// `GetParam`.
    pub(crate) fn get_param(&self, identifier: &[u8], param_type: ParamType) -> Result<Option<u64>> {
        self.param_type_check(param_type)?;
        Ok(self.named_params.get(identifier).copied())
    }

    // ------------------------------------------------------------------------------
    // CTEs and pivots
    // ------------------------------------------------------------------------------

    /// `ExtractCTEsRecursive`: copies of the CTEs visible here, innermost first; a name
    /// defined twice keeps its innermost definition.
    pub(crate) fn extract_ctes(&self, cte_map: &mut CommonTableExpressionMap) {
        for frame in self.frames_inside_out() {
            for stored in &frame.stored_cte_maps {
                for (name, info) in stored.map.iter() {
                    if !cte_map.map.contains_key(name) {
                        cte_map.map.insert(name, info.clone());
                    }
                }
            }
        }
    }

    /// Whether the query mentions PIVOT (and so may need the stored CTEs).
    pub(crate) fn may_pivot(&mut self) -> bool {
        *self
            .may_pivot
            .get_or_insert_with(|| contains_ignore_ascii_case(self.query, b"pivot"))
    }

    /// `PivotEntryCheck`: CREATE VIEW / MACRO cannot use a PIVOT that reads its values
    /// from the data.
    pub(crate) fn pivot_entry_check(&self, kind: &str) -> Result<()> {
        if let Some(entry) = self.pivot_entries.first() {
            return Err(Error::parser(roastedduck_common::byte_format!(
                "PIVOT statements with pivot elements extracted from the data cannot be used in {kind}s.\nIn order \
                 to use PIVOT in a {kind} the PIVOT values must be manually specified, e.g.:\nPIVOT ... ON {} IN \
                 (val1, val2, ...)",
                entry.column
            )));
        }
        Ok(())
    }

    /// `ParamCount`.
    pub(crate) const fn param_count(&self) -> u64 {
        self.param_count
    }
}

/// Whether `haystack` contains `needle`, ignoring ASCII case.
fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

// ----------------------------------------------------------------------------------
// Reading the Postgres tree
// ----------------------------------------------------------------------------------

/// A pointer DuckDB dereferences unconditionally: missing means a malformed tree.
pub(crate) fn required<'b, T>(value: Option<&'b T>, what: &str) -> Result<&'b T> {
    value.ok_or_else(|| {
        Error::internal(roastedduck_common::byte_format!(
            "Unexpected NULL {what} in the parse tree"
        ))
    })
}

/// The text of a `PGString` value node, or of a bare C string in a list.
pub(crate) fn node_string(node: &Node) -> Result<&[u8]> {
    match node {
        Node::Value(Value::String(text)) | Node::Str(text) => Ok(text),
        other => Err(Error::internal(roastedduck_common::byte_format!(
            "Expected a string in the parse tree, found {}",
            node_tag(other).1
        ))),
    }
}

/// `TransformStringList` / `TransformNameList`: the strings of a list.
pub(crate) fn string_list(list: &List) -> Result<Vec<ByteString>> {
    list.iter()
        .map(|node| node_string(node).map(ByteString::from))
        .collect()
}

/// The `name` DuckDB reads from a list cell it treats as a `PGResTarget`. Some grammar
/// rules put plain strings in such lists; `PGResTarget::name` and `PGValue::val.str`
/// share their offset, so DuckDB reads the string.
pub(crate) fn res_target_name(node: &Node) -> Result<Option<ByteString>> {
    match node {
        Node::ResTarget(target) => Ok(target.name.clone()),
        Node::Value(Value::String(text)) | Node::Str(text) => Ok(Some(text.clone())),
        other => Err(Error::internal(roastedduck_common::byte_format!(
            "Expected a ResTarget node in the parse tree, found {}",
            node_tag(other).1
        ))),
    }
}

/// Borrows `node` as the node struct `T`, failing like a bad `PGCast` would.
pub(crate) fn cast_node<T: roastedduck_pg_parser::nodes::NodeStruct>(node: &Node) -> Result<&T> {
    node.downcast_ref::<T>().ok_or_else(|| {
        Error::internal(roastedduck_common::byte_format!(
            "Expected a {} node in the parse tree, found {}",
            T::NAME,
            node_tag(node).1
        ))
    })
}

/// A parser error positioned at an expression's location (`ParserException(location, ...)`).
pub(crate) fn parser_error_at(location: Option<u64>, message: impl Into<ByteString>) -> Error {
    Error::parser(message).with_position(location)
}
