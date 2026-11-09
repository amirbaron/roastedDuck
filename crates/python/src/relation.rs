//! Native relational query trees and independent result cursors over an owned Python connection.
use crate::connection::{FetchedRow, PyConnection, ResultCursor, RowCheckpoint, State, lock};
use crate::datatype::PyType;
use crate::pending::PythonExecution;
use crate::{conversion, error};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyList, PyString, PyTuple};
use roastedduck::{Connection, LogicalType, MaterializedResult, QueryResult};
use roastedduck_ast::{
    AggregateHandling, BaseTableRef, ColumnDataRef, CreateInfoKind, JoinRefType, JoinType,
    ParsedExpressionKind, QueryNodeKind, SelectStatement, SetOperationNode, SetOperationType, SqlStatement,
    SqlStatementKind, SubqueryRef, TableRef, TableRefKind,
};
use roastedduck_common::{ByteString, Error, Result, byte_format};
use roastedduck_types::{ColumnDataCollection, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
static ALIASES: AtomicU64 = AtomicU64::new(0);
fn next_alias() -> ByteString {
    format!(
        "unnamed_relation_{:016x}",
        ALIASES.fetch_add(1, Ordering::Relaxed)
    )
    .into()
}
fn statement(select: SelectStatement) -> SqlStatement {
    let query = select.to_sql_bytes();
    SqlStatement {
        query,
        ..SqlStatement::new(SqlStatementKind::Select(select))
    }
}
fn select(statement: SqlStatement) -> Result<SelectStatement> {
    match statement.kind {
        SqlStatementKind::Select(select) => Ok(select),
        _ => Err(Error::invalid_input("Expected a SELECT relation")),
    }
}
fn parse(sql: &[u8]) -> Result<SelectStatement> {
    let mut statements = roastedduck_parser::parse_bytes(sql)?;
    if statements.len() != 1 {
        return Err(Error::invalid_input("Expected a single SELECT relation"));
    }
    select(statements.remove(0))
}
fn run(connection: &mut Connection, select: SelectStatement, stream: bool) -> Result<QueryResult> {
    let statement = statement(select);
    let query = statement.query.clone();
    let pending = connection.pending_query(query, statement, stream)?;
    crate::pending::complete(connection, &pending)
}
fn execute_insert(
    connection: &mut Connection,
    query: &[u8],
    statement: SqlStatement,
    table: &[u8],
) -> Result<()> {
    // InsertRel binds before Execute. Only execution errors get Relation::Insert's prefix.
    let prefix = |error: Error| {
        Error::new_bytes(
            error.kind(),
            byte_format!(
                "Failed to insert into table '{}': {}",
                table,
                error.raw_message_bytes()
            )
            .into_bytes(),
        )
    };
    let pending = connection
        .pending_query(query, statement, false)
        .map_err(|error| {
            // The native optimizer can evaluate a constant cast while starting the pending
            // query. C++ reports this conversion failure from InsertRelation::Execute.
            if error.kind() == roastedduck_common::ErrorKind::Conversion {
                prefix(error)
            } else {
                error
            }
        })?;
    crate::pending::complete(connection, &pending)
        .map(drop)
        .map_err(|error| {
            if error.subtype() == Some("python_interrupt") {
                return error;
            }
            Error::new_bytes(
                error.kind(),
                byte_format!(
                    "Failed to insert into table '{}': {}",
                    table,
                    error.raw_message_bytes()
                )
                .into_bytes(),
            )
        })
}
fn materialized_select(result: &MaterializedResult) -> Result<SelectStatement> {
    let mut node = roastedduck_parser::build::select_node();
    node.select_list.push(roastedduck_parser::build::star());
    *node.from_table = roastedduck_parser::build::table_ref(TableRefKind::ColumnData(ColumnDataRef {
        expected_names: result.names().to_vec(),
        collection: Some(Box::new(ColumnDataCollection::new(
            result.types().to_vec(),
            result.collect_chunks()?,
        ))),
    }));
    Ok(roastedduck_parser::build::select_statement(
        roastedduck_parser::build::query_node(QueryNodeKind::Select(Box::new(node))),
    ))
}
pub(super) fn qualified_name(py: Python<'_>, name: impl AsRef<[u8]>) -> PyResult<[ByteString; 3]> {
    let name = name.as_ref();
    let mut parts: Vec<ByteString> = Vec::new();
    {
        let mut quoted = false;
        let mut entry = Vec::new();
        for &byte in name {
            match byte {
                b'"' => quoted = !quoted,
                b'.' if !quoted => parts.push(std::mem::take(&mut entry).into()),
                _ => entry.push(byte),
            }
        }
        if quoted {
            return Err(error::native(
                py,
                &Error::parser(byte_format!(
                    "Unterminated quote in qualified name! (input: {})",
                    name
                )),
            ));
        }
        if !entry.is_empty() {
            parts.push(entry.into());
        }
        if parts.len() > 3 {
            return Err(error::native(
                py,
                &Error::parser(byte_format!(
                    "Expected catalog.entry, schema.entry or entry: too many entries found (input: {})",
                    name
                )),
            ));
        }
    }
    let name = parts.pop().unwrap_or_default();
    let schema = parts.pop().unwrap_or_default();
    let catalog = parts.pop().unwrap_or_default();
    Ok([catalog, schema, name])
}
fn quoted(name: &[u8]) -> ByteString {
    let mut result = Vec::with_capacity(name.len() + 2);
    result.push(b'"');
    for &byte in name {
        result.push(byte);
        if byte == b'"' {
            result.push(byte);
        }
    }
    result.push(b'"');
    result.into()
}
#[derive(Default)]
struct RenderOutput(Vec<u8>);
impl roastedduck_cli::box_renderer::BaseResultRenderer for RenderOutput {
    fn render_layout(&mut self, t: &[u8]) {
        self.0.extend_from_slice(t);
    }
    fn render_column_name(&mut self, t: &[u8]) {
        self.0.extend_from_slice(t);
    }
    fn render_type(&mut self, t: &[u8]) {
        self.0.extend_from_slice(t);
    }
    fn render_value(&mut self, t: &[u8], _: &LogicalType) {
        self.0.extend_from_slice(t);
    }
    fn render_null(&mut self, t: &[u8], _: &LogicalType) {
        self.0.extend_from_slice(t);
    }
    fn render_footer(&mut self, t: &[u8]) {
        self.0.extend_from_slice(t);
    }
}
#[derive(Debug, Default)]
struct Cursor {
    result: Option<ResultCursor>,
    closed: bool,
    executed: bool,
}
#[pyclass(module = "_duckdb", name = "DuckDBPyRelation", frozen)]
#[derive(Debug)]
pub struct PyRelation {
    owner: Py<PyConnection>,
    state: Arc<Mutex<State>>,
    source: SelectStatement,
    names: Vec<ByteString>,
    types: Vec<LogicalType>,
    alias: ByteString,
    kind: &'static str,
    join_bindings: bool,
    cursor: Mutex<Cursor>,
    rendered: Mutex<ByteString>,
}
impl PyRelation {
    pub(super) fn connection_id(&self, py: Python<'_>) -> u64 {
        self.owner.bind(py).get().id
    }
    pub(super) fn replacement(&self, object: &Bound<'_, PyAny>, alias: &[u8]) -> TableRef {
        let mut table = roastedduck_parser::build::table_ref(TableRefKind::Subquery(SubqueryRef {
            subquery: Box::new(self.source.clone()),
            column_name_alias: Vec::new(),
        }));
        table.alias = alias.into();
        table.external_dependency =
            roastedduck_ast::ExternalDependency::new(Arc::new(object.clone().unbind()));
        table
    }
    pub(super) fn from_values(owner: Bound<'_, PyConnection>, args: &Bound<'_, PyTuple>) -> PyResult<Self> {
        let py = owner.py();
        let state = Arc::clone(&owner.get().state);
        py.detach(|| lock(&state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        if args.is_empty() {
            return Err(error::native(
                py,
                &Error::invalid_input("Could not create a ValueRelation without any inputs"),
            ));
        }
        let first = args.get_item(0)?;
        let values_only = args.len() == 1 && first.is_instance_of::<PyList>();
        let expressions = |row: &Bound<'_, PyTuple>| -> PyResult<Vec<roastedduck_ast::ParsedExpression>> {
            if row.is_empty() {
                return Err(error::native(
                    py,
                    &Error::invalid_input("Please provide a non-empty tuple"),
                ));
            }
            row.iter()
                .map(|value| {
                    crate::expression::PyExpression::from_object(&value)
                        .map(|e| e.native)
                        .map_err(|_| {
                            error::native(
                                py,
                                &Error::invalid_input("Please provide arguments of type Expression!"),
                            )
                        })
                })
                .collect()
        };
        let rows = if values_only {
            vec![
                first
                    .try_iter()?
                    .map(|v| conversion::from_python(&v?).map(roastedduck_parser::build::constant))
                    .collect::<PyResult<Vec<_>>>()?,
            ]
        } else if first.is_instance_of::<PyTuple>() {
            let mut rows = Vec::new();
            let mut length = 0;
            for value in args.iter() {
                let Ok(row) = value.cast::<PyTuple>() else {
                    return Err(error::native(
                        py,
                        &Error::invalid_input(format!(
                            "Expected objects of type tuple, not {}",
                            value.get_type().str()?
                        )),
                    ));
                };
                let row = expressions(row)?;
                if !rows.is_empty() && row.len() != length {
                    return Err(error::native(
                        py,
                        &Error::invalid_input(format!(
                            "Mismatch between length of tuples in input, expected {length} but found {}",
                            row.len()
                        )),
                    ));
                }
                length = row.len();
                rows.push(row);
            }
            rows
        } else {
            vec![expressions(args)?]
        };
        let names = if values_only {
            Vec::new()
        } else {
            crate::host::names(
                &rows[0]
                    .iter()
                    .map(roastedduck_ast::ParsedExpression::name)
                    .collect::<Vec<_>>(),
            )
        };
        let table = roastedduck_parser::build::table_ref(TableRefKind::ExpressionList(Box::new(
            roastedduck_ast::ExpressionListRef {
                expected_names: names,
                expected_types: Vec::new(),
                values: rows,
            },
        )));
        let mut source = roastedduck_parser::build::select_node();
        source.select_list.push(roastedduck_parser::build::star());
        *source.from_table = table;
        source.from_table.alias = "values".into();
        let source = roastedduck_parser::build::select_statement(roastedduck_parser::build::query_node(
            QueryNodeKind::Select(Box::new(source)),
        ));
        let mut relation = Self::bind(
            py,
            owner.unbind(),
            state,
            source,
            "values".into(),
            "VALUE_LIST_RELATION",
        )?;
        if let QueryNodeKind::Select(node) = &mut relation.source.node.kind
            && let TableRefKind::ExpressionList(values) = &mut node.from_table.kind
        {
            values.expected_names.clone_from(&relation.names);
            values.expected_types.clone_from(&relation.types);
        }
        Ok(relation)
    }
    #[allow(clippy::too_many_arguments)]
    fn aggregation(
        &self,
        py: Python<'_>,
        function: &str,
        columns: &str,
        parameters: &str,
        groups: &str,
        window: &str,
        projected: &str,
        ignore_nulls: bool,
    ) -> PyResult<Self> {
        if !groups.is_empty() && !window.is_empty() {
            return Err(error::native(
                py,
                &Error::invalid_input("Either groups or window must be set (can't be both at the same time)"),
            ));
        }
        let expression = crate::aggregation::expressions(
            py,
            function,
            columns,
            parameters,
            projected,
            window,
            ignore_nulls,
        )?;
        if window.is_empty() {
            self.aggregate(py, PyString::new(py, &expression).as_any(), groups)
        } else {
            self.derive(
                py,
                &byte_format!("SELECT {} FROM __relation", expression.as_bytes()),
                "PROJECTION_RELATION",
            )
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn window_function(
        &self,
        py: Python<'_>,
        function: &str,
        parameters: &str,
        columns: &str,
        window: &str,
        ignore_nulls: bool,
        projected: &str,
    ) -> PyResult<Self> {
        let expression = crate::aggregation::expressions(
            py,
            function,
            columns,
            parameters,
            projected,
            window,
            ignore_nulls,
        )?;
        self.derive(
            py,
            &byte_format!("SELECT {} FROM __relation", expression.as_bytes()),
            "PROJECTION_RELATION",
        )
    }
    fn render(
        &self,
        py: Python<'_>,
        config: roastedduck_cli::box_renderer::BoxRendererConfig,
        invalidate: bool,
    ) -> PyResult<Py<PyAny>> {
        if !invalidate {
            // C++ keeps an already rendered representation readable after close.
            // Release the GIL while waiting for another renderer's cache lock.
            let cached = py.detach(|| lock(&self.rendered).clone());
            if !cached.is_empty() {
                return conversion::text(py, &cached);
            }
        }
        let rendered = py
            .detach(|| {
                let state = lock(&self.state);
                let mut connection = state.relation_connection()?;
                let mut cached = lock(&self.rendered);
                if cached.is_empty() || invalidate {
                    let mut source = parse(b"SELECT * FROM __relation LIMIT 10000")?;
                    if let QueryNodeKind::Select(node) = &mut source.node.kind {
                        *node.from_table = self.table_ref();
                    }
                    let result = match run(&mut connection, source, false)? {
                        QueryResult::Materialized(result) => result,
                        QueryResult::Stream(stream) => connection.materialize(&stream)?,
                    };
                    let result = Arc::new(result);
                    let mut renderer = roastedduck_cli::box_renderer::BoxRendererState::prepare(
                        config,
                        &*connection,
                        result.names(),
                        Arc::clone(&result),
                    )?;
                    let mut output = RenderOutput::default();
                    renderer.render(&mut output, &*connection)?;
                    *cached = output.0.into();
                }
                Ok::<_, Error>(cached.clone())
            })
            .map_err(|e| error::native(py, &e))?;
        conversion::text(py, &rendered)
    }
    fn arrow_reader(&self, py: Python<'_>, batch_size: usize, capsule: bool) -> PyResult<Py<PyAny>> {
        let (connection, result) = py
            .detach(|| {
                let state = lock(&self.state);
                let mut connection = state.relation_connection()?;
                let handle = state.reader_handle()?;
                let mut cursor = lock(&self.cursor);
                if cursor.closed {
                    return Err(Error::invalid_input("There is no query result"));
                }
                self.ensure(&mut connection, &mut cursor, true)?;
                let result = cursor
                    .result
                    .take()
                    .ok_or_else(|| Error::internal("Missing relation result"))?;
                if batch_size == 0 {
                    cursor.closed = true;
                }
                Ok::<_, Error>((handle, result))
            })
            .map_err(|e| error::native(py, &e))?;
        crate::reader::export(py, connection, result, batch_size, capsule)
    }
    pub(super) fn from_table_ref(owner: Bound<'_, PyConnection>, table: TableRef) -> PyResult<Self> {
        let py = owner.py();
        let alias = table.alias.clone();
        let mut node = roastedduck_parser::build::select_node();
        node.select_list.push(roastedduck_parser::build::star());
        *node.from_table = table;
        let source = roastedduck_parser::build::select_statement(roastedduck_parser::build::query_node(
            QueryNodeKind::Select(Box::new(node)),
        ));
        let state = Arc::clone(&owner.get().state);
        Self::bind(py, owner.unbind(), state, source, alias, "VIEW_RELATION")
    }

    pub(super) fn from_json_table(
        owner: Bound<'_, PyConnection>,
        table: TableRef,
        alias: ByteString,
    ) -> PyResult<Self> {
        let mut relation = Self::from_table_ref(owner, table)?;
        relation.alias = alias;
        relation.kind = "TABLE_FUNCTION_RELATION";
        Ok(relation)
    }
    pub(super) fn from_parquet_table(owner: Bound<'_, PyConnection>, table: TableRef) -> PyResult<Self> {
        let mut relation = Self::from_table_ref(owner, table)?;
        relation.kind = "SUBQUERY_RELATION";
        Ok(relation)
    }
    pub(super) fn from_file_table(
        owner: Bound<'_, PyConnection>,
        table: TableRef,
        names: Vec<ByteString>,
        types: Vec<LogicalType>,
    ) -> PyResult<Self> {
        let mut relation = Self::from_table_ref(owner, table)?;
        relation.kind = "SUBQUERY_RELATION";
        relation.names = names;
        relation.types = types;
        Ok(relation)
    }
    fn write_file(
        &self,
        py: Python<'_>,
        file_name: &str,
        format: &str,
        options: roastedduck_common::CaseInsensitiveMap<Vec<Value>, ByteString>,
    ) -> PyResult<()> {
        let statement = SqlStatement::new(SqlStatementKind::Copy(Box::new(roastedduck_ast::CopyInfo {
            schema: ByteString::new(),
            catalog: ByteString::new(),
            table: ByteString::new(),
            select_list: Vec::new(),
            is_from: false,
            format: format.into(),
            file_path: file_name.into(),
            options,
            select_statement: Some(self.source.node.clone()),
            is_format_auto_detected: false,
            parsed_options: roastedduck_common::CaseInsensitiveMap::default(),
            file_path_expression: None,
        })));
        py.detach(|| {
            lock(&self.state)
                .relation_connection()?
                .execute_python_statement([], statement)
                .map(|_| ())
        })
        .map_err(|e| error::native(py, &e))
    }
    pub(super) fn from_table_function(owner: Bound<'_, PyConnection>, table: TableRef) -> PyResult<Self> {
        let alias = if let TableRefKind::TableFunction(function) = &table.kind
            && let ParsedExpressionKind::Function(function) = &function.function.kind
        {
            function.function_name.clone()
        } else {
            table.alias.clone()
        };
        let mut relation = Self::from_table_ref(owner, table)?;
        relation.alias = alias;
        relation.kind = "TABLE_FUNCTION_RELATION";
        Ok(relation)
    }
    fn fetch_arrow(&self, py: Python<'_>, rows_per_batch: usize) -> PyResult<Py<PyAny>> {
        self.fetch_arrow_named(py, rows_per_batch, false)
    }
    fn fetch_arrow_named(
        &self,
        py: Python<'_>,
        rows_per_batch: usize,
        deduplicate: bool,
    ) -> PyResult<Py<PyAny>> {
        let mut metadata = py
            .detach(|| {
                let state = lock(&self.state);
                let mut connection = state.relation_connection()?;
                let mut cursor = lock(&self.cursor);
                if cursor.closed {
                    return Err(Error::invalid_input("There is no query result"));
                }
                self.ensure(&mut connection, &mut cursor, false)?;
                Ok::<_, Error>(
                    cursor
                        .result
                        .as_mut()
                        .ok_or_else(|| Error::internal("Missing relation result"))?
                        .begin_bulk(),
                )
            })
            .map_err(|e| error::native(py, &e))?;
        if deduplicate {
            metadata.names = crate::host::names(&metadata.names);
        }
        let output = crate::host::arrow(py, &metadata, rows_per_batch, || {
            py.detach(|| {
                let state = lock(&self.state);
                let mut connection = state.relation_connection()?;
                lock(&self.cursor)
                    .result
                    .as_mut()
                    .ok_or_else(|| Error::internal("Missing relation result"))?
                    .bulk_next(&mut connection)
            })
            .map_err(|e| error::native(py, &e))
        })?;
        py.detach(|| {
            let state = lock(&self.state);
            if let Ok(mut connection) = state.relation_connection() {
                Self::clear(&mut connection, &mut lock(&self.cursor));
            }
        });
        Ok(output)
    }
    fn fetch_host(&self, py: Python<'_>, pandas: bool, date_as_object: bool) -> PyResult<Py<PyAny>> {
        self.fetch_host_limit(py, pandas, date_as_object, None)
    }
    fn fetch_host_limit(
        &self,
        py: Python<'_>,
        pandas: bool,
        date_as_object: bool,
        max_chunks: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let metadata = py
            .detach(|| {
                let state = lock(&self.state);
                let mut connection = state.relation_connection()?;
                let mut cursor = lock(&self.cursor);
                self.ensure(&mut connection, &mut cursor, max_chunks.is_some())?;
                Ok::<_, Error>(
                    cursor
                        .result
                        .as_mut()
                        .ok_or_else(|| Error::internal("Missing relation result"))?
                        .begin_bulk(),
                )
            })
            .map_err(|e| error::native(py, &e))?;
        let output = crate::host::fetch(py, &metadata, pandas, pandas, date_as_object, max_chunks, || {
            py.detach(|| {
                let state = lock(&self.state);
                let mut connection = state.relation_connection()?;
                lock(&self.cursor)
                    .result
                    .as_mut()
                    .ok_or_else(|| Error::internal("Missing relation result"))?
                    .bulk_next(&mut connection)
            })
            .map_err(|e| error::native(py, &e))
        })?;
        if max_chunks.is_none() {
            py.detach(|| {
                let state = lock(&self.state);
                if let Ok(mut connection) = state.relation_connection() {
                    Self::clear(&mut connection, &mut lock(&self.cursor));
                }
            });
        }
        Ok(output)
    }
    fn replacement_table(owner: Bound<'_, PyConnection>, name: &[u8]) -> PyResult<Self> {
        // C++ RunQuery receives a Python string, so its fallback must perform the
        // same UTF-8 conversion even though catalog identifiers themselves are bytes.
        let sql = byte_format!(
            "from {}",
            roastedduck_ast::keyword::write_optionally_quoted_bytes(name)
        );
        let py = owner.py();
        let query = conversion::text(py, &sql)?;
        Self::from_input(owner, crate::statement::input(query.bind(py))?, name, None)?
            .ok_or_else(|| error::InternalException::new_err("Expected table relation"))
    }

    pub(super) fn from_table(
        owner: Bound<'_, PyConnection>,
        name: impl AsRef<[u8]>,
        view: bool,
    ) -> PyResult<Self> {
        let name = name.as_ref();
        let py = owner.py();
        let [catalog_name, mut schema_name, table_name] = if view {
            [ByteString::new(), "main".into(), name.into()]
        } else {
            qualified_name(py, name)?
        };
        if schema_name.is_empty() {
            schema_name = "main".into();
        }
        if !view {
            let state = Arc::clone(&owner.get().state);
            let found = py.detach(|| {
                lock(&state).relation_connection()?.relation_is_base_table(
                    &catalog_name,
                    &schema_name,
                    &table_name,
                )
            });
            match found {
                Ok(true) => {}
                Ok(false) => return Self::replacement_table(owner, name),
                Err(error) if error.kind() == roastedduck_common::ErrorKind::Catalog => {
                    return Self::replacement_table(owner, name);
                }
                Err(error) => return Err(crate::error::native(py, &error)),
            }
        }
        let mut node = roastedduck_parser::build::select_node();
        node.select_list.push(roastedduck_parser::build::star());
        *node.from_table =
            roastedduck_parser::build::table_ref(TableRefKind::BaseTable(Box::new(BaseTableRef {
                table_name: table_name.clone(),
                schema_name,
                catalog_name,
                column_name_alias: Vec::new(),
                at_clause: None,
            })));
        // A programmatically constructed table has no parser location. Give its
        // sole reference a stable position so relation binding can retain the
        // injected external scan just as it does for parsed SELECT statements.
        node.from_table.query_location = Some(0);
        let source = roastedduck_parser::build::select_statement(roastedduck_parser::build::query_node(
            QueryNodeKind::Select(Box::new(node)),
        ));
        let state = Arc::clone(&owner.get().state);
        match Self::bind(
            py,
            owner.clone().unbind(),
            state,
            source,
            table_name,
            if view { "VIEW_RELATION" } else { "TABLE_RELATION" },
        ) {
            Ok(_) if view && name.is_empty() => Err(error::native(
                py,
                &Error::internal("Calling BindingAlias::GetAlias on a non-set alias"),
            )),
            Err(err) if !view && err.is_instance_of::<error::CatalogException>(py) => {
                // C++ retries the entire input as one identifier for replacement scans.
                Self::replacement_table(owner, name)
            }
            result => result,
        }
    }
    fn bind(
        py: Python<'_>,
        owner: Py<PyConnection>,
        state: Arc<Mutex<State>>,
        source: SelectStatement,
        alias: ByteString,
        kind: &'static str,
    ) -> PyResult<Self> {
        let (names, types, source) = py
            .detach(|| {
                let state = lock(&state);
                let mut statement = statement(source);
                let (names, types) = state.relation_connection()?.relation_columns(&mut statement)?;
                Ok::<_, Error>((names, types, select(statement)?))
            })
            .map_err(|e| error::native(py, &e))?;
        let join_bindings = matches!(
            kind,
            "JOIN_RELATION" | "FILTER_RELATION" | "ORDER_RELATION" | "LIMIT_RELATION" | "DISTINCT_RELATION"
        ) && matches!(&source.node.kind, QueryNodeKind::Select(node) if matches!(node.from_table.kind, TableRefKind::Join(_)));
        Ok(Self {
            owner,
            state,
            source,
            names,
            types,
            alias,
            kind,
            join_bindings,
            cursor: Mutex::default(),
            rendered: Mutex::default(),
        })
    }
    pub(super) fn from_sql(
        owner: Bound<'_, PyConnection>,
        sql: impl AsRef<[u8]>,
        alias: &str,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Self>> {
        Self::from_input(owner, crate::statement::Query::Text(sql.as_ref()), alias, params)
    }
    pub(super) fn from_input(
        owner: Bound<'_, PyConnection>,
        input: crate::statement::Query<'_>,
        alias: impl AsRef<[u8]>,
        params: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Self>> {
        let alias = alias.as_ref();
        let py = owner.py();
        let state = Arc::clone(&owner.get().state);
        let values = PyConnection::parameters(params)?.filter(|values| !values.is_empty());
        let source = py
            .detach(|| {
                let state = lock(&state);
                let mut connection = state.connection()?;
                let mut statements = input.statements(&mut connection)?;
                let Some(last) = statements.pop() else {
                    return Ok(None);
                };
                for statement in statements {
                    let query = statement.query.clone();
                    connection.execute_python_statement(query, statement)?;
                }
                if values.is_none() && matches!(last.kind, SqlStatementKind::Select(_)) {
                    if !last.named_param_map.is_empty() {
                        return Err(Error::binder(
                            "Unexpected prepared parameter. This type of statement can't be prepared!",
                        ));
                    }
                    return select(last).map(Some);
                }
                let result = if let Some(values) = &values {
                    let prepared = connection.prepare_statement(last)?;
                    connection.execute_python_prepared(&prepared, values, false)?
                } else {
                    let query = last.query.clone();
                    QueryResult::Materialized(connection.execute_python_statement(query, last)?)
                };
                let result = match result {
                    QueryResult::Materialized(result) => result,
                    QueryResult::Stream(stream) => connection.materialize(&stream)?,
                };
                if result.return_type() != roastedduck::StatementReturnType::QueryResult {
                    return Ok(None);
                }
                materialized_select(&result).map(Some)
            })
            .map_err(|e| error::native(py, &e))?;
        let Some(source) = source else {
            return Ok(None);
        };
        let alias = if alias.is_empty() {
            next_alias()
        } else {
            alias.into()
        };
        Self::bind(py, owner.unbind(), state, source, alias, "QUERY_RELATION").map(Some)
    }
    fn table_ref(&self) -> TableRef {
        if matches!(self.kind, "JOIN_RELATION" | "CROSS_PRODUCT_RELATION")
            && let QueryNodeKind::Select(node) = &self.source.node.kind
        {
            return (*node.from_table).clone();
        }
        let mut table = roastedduck_parser::build::table_ref(TableRefKind::Subquery(SubqueryRef {
            subquery: Box::new(self.source.clone()),
            column_name_alias: Vec::new(),
        }));
        table.alias = self.alias.clone();
        table
    }
    fn assert_open(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))
    }
    fn set_operation(&self, py: Python<'_>, other: &Self, setop_type: SetOperationType) -> PyResult<Self> {
        self.assert_open(py)?;
        other.assert_open(py)?;
        if !Arc::ptr_eq(&self.state, &other.state) {
            return Err(error::native(
                py,
                &Error::invalid_input("Cannot combine LEFT and RIGHT relations of different connections!"),
            ));
        }
        let children = [self, other].map(|relation| {
            let mut node = roastedduck_parser::build::select_node();
            node.select_list.push(roastedduck_parser::build::star());
            *node.from_table = relation.table_ref();
            roastedduck_parser::build::query_node(QueryNodeKind::Select(Box::new(node)))
        });
        let source = roastedduck_parser::build::select_statement(roastedduck_parser::build::query_node(
            QueryNodeKind::SetOperation(SetOperationNode {
                setop_type,
                setop_all: true,
                children: children.into(),
            }),
        ));
        Self::bind(
            py,
            self.owner.clone_ref(py),
            Arc::clone(&self.state),
            source,
            self.alias.clone(),
            "SET_OPERATION_RELATION",
        )
    }
    fn derive(&self, py: Python<'_>, template: &[u8], kind: &'static str) -> PyResult<Self> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        self.derive_select(py, parse(template).map_err(|e| error::native(py, &e))?, kind)
    }
    fn derive_select(
        &self,
        py: Python<'_>,
        mut select: SelectStatement,
        kind: &'static str,
    ) -> PyResult<Self> {
        let QueryNodeKind::Select(node) = &mut select.node.kind else {
            return Err(error::InternalException::new_err(
                "Expected SELECT relation template",
            ));
        };
        if kind == "PROJECTION_RELATION" {
            node.aggregate_handling = AggregateHandling::NoAggregatesAllowed;
        }
        if self.join_bindings {
            let mut source = self.source.clone();
            let QueryNodeKind::Select(child) = &mut source.node.kind else {
                return Err(error::InternalException::new_err("Expected join SELECT"));
            };
            match kind {
                "PROJECTION_RELATION" => {
                    child.select_list.clone_from(&node.select_list);
                    child.aggregate_handling = AggregateHandling::NoAggregatesAllowed;
                }
                "AGGREGATE_RELATION" => {
                    child.select_list.clone_from(&node.select_list);
                    child.group_expressions.clone_from(&node.group_expressions);
                    child.group_sets.clone_from(&node.group_sets);
                    child.aggregate_handling = node.aggregate_handling;
                }
                "FILTER_RELATION" => {
                    if let Some(condition) = node.where_clause.take() {
                        child.where_clause = Some(if let Some(previous) = child.where_clause.take() {
                            Box::new(roastedduck_parser::build::conjunction(
                                roastedduck_ast::ExpressionType::ConjunctionAnd,
                                *previous,
                                *condition,
                            ))
                        } else {
                            condition
                        });
                    }
                }
                "LIMIT_RELATION" | "DISTINCT_RELATION" => {
                    source.node.modifiers.append(&mut select.node.modifiers);
                }
                _ => {
                    *node.from_table = self.table_ref();
                    source = select;
                }
            }
            return Self::bind(
                py,
                self.owner.clone_ref(py),
                Arc::clone(&self.state),
                source,
                self.alias.clone(),
                kind,
            );
        }
        *node.from_table = self.table_ref();
        Self::bind(
            py,
            self.owner.clone_ref(py),
            Arc::clone(&self.state),
            select,
            self.alias.clone(),
            kind,
        )
    }
    fn clear(connection: &mut Connection, cursor: &mut Cursor) {
        if let Some(result) = cursor.result.take() {
            result.close(connection);
        }
    }
    fn ensure(&self, connection: &mut Connection, cursor: &mut Cursor, stream: bool) -> Result<()> {
        if cursor.closed {
            return Err(Error::invalid_input("result closed"));
        }
        if cursor.result.is_none() {
            cursor.result = Some(ResultCursor::new(
                run(connection, self.source.clone(), stream)?,
                connection.client_properties(),
            ));
            cursor.executed = true;
        }
        Ok(())
    }
    fn next_row(&self, stream: bool) -> Result<FetchedRow> {
        let state = lock(&self.state);
        let mut connection = state.relation_connection()?;
        let mut cursor = lock(&self.cursor);
        self.ensure(&mut connection, &mut cursor, stream)?;
        let row = match &mut cursor.result {
            Some(result) => result.next(&mut connection)?,
            None => None,
        };
        let zone = cursor
            .result
            .as_ref()
            .ok_or_else(|| Error::internal("Missing relation result"))?
            .timezone()
            .to_owned();
        let checkpoint = row
            .as_ref()
            .and_then(|_| cursor.result.as_ref().map(ResultCursor::row_checkpoint));
        Ok((row, zone, checkpoint))
    }
    fn row_to_python(
        &self,
        py: Python<'_>,
        row: &[Value],
        timezone: &str,
        checkpoint: Option<RowCheckpoint>,
    ) -> PyResult<Py<PyAny>> {
        let converted = row
            .iter()
            .map(|value| conversion::to_python(py, value, timezone))
            .collect::<PyResult<Vec<_>>>()
            .and_then(|values| PyTuple::new(py, values).map(|tuple| tuple.unbind().into_any()));
        if converted.is_err() {
            py.detach(|| {
                if let Some(checkpoint) = checkpoint
                    && let Some(result) = lock(&self.cursor).result.as_mut()
                {
                    result.restore_row(checkpoint);
                }
            });
        }
        converted
    }
    fn rows(&self, py: Python<'_>, limit: Option<usize>) -> PyResult<Py<PyAny>> {
        let output = PyList::empty(py);
        let mut count = 0;
        while limit.is_none_or(|limit| count < limit) {
            let (row, zone, checkpoint) = py
                .detach(|| self.next_row(limit.is_some()))
                .map_err(|e| error::native(py, &e))?;
            let Some(row) = row else {
                break;
            };
            output.append(self.row_to_python(py, &row, &zone, checkpoint)?)?;
            count += 1;
        }
        if limit.is_none() {
            py.detach(|| {
                let state = lock(&self.state);
                if let Ok(mut connection) = state.connection() {
                    Self::clear(&mut connection, &mut lock(&self.cursor));
                }
            });
        }
        Ok(output.unbind().into_any())
    }
}
impl Drop for PyRelation {
    fn drop(&mut self) {
        if let Ok(state) = self.state.try_lock()
            && let Ok(mut connection) = state.connection()
        {
            let cursor = self
                .cursor
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::clear(&mut connection, cursor);
        }
    }
}
#[pymethods]
impl PyRelation {
    #[pyo3(signature = (map_function, *, schema=None))]
    fn map(
        &self,
        py: Python<'_>,
        map_function: &Bound<'_, PyAny>,
        schema: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        if !map_function.is_callable() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "map(): map_function must be callable",
            ));
        }
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        let table = crate::map::table(self.source.clone(), map_function, schema);
        let mut relation = Self::from_table_ref(self.owner.bind(py).clone(), table)?;
        relation.kind = "TABLE_FUNCTION_RELATION";
        Ok(relation)
    }
    fn __str__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.render(
            py,
            roastedduck_cli::box_renderer::BoxRendererConfig {
                limit: 10000,
                ..Default::default()
            },
            false,
        )
    }
    fn __repr__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.__str__(py)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, *, max_width=None, max_rows=None, max_col_width=None, null_value=None, render_mode=None)")]
    fn show(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        use roastedduck_cli::box_renderer::{BoxRenderMode, BoxRendererConfig};
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyRelation, *, max_width: typing.Optional[int] = None, max_rows: typing.Optional[int] = None, max_col_width: typing.Optional[int] = None, null_value: typing.Optional[str] = None, render_mode: object = None) -> None";
        let bad = || error::arguments(py, "show", signature, Some(slf.as_any()), args, kwargs);
        let fields = crate::arguments::fields(
            "show",
            signature,
            slf.as_any(),
            args,
            kwargs,
            &[
                "max_width",
                "max_rows",
                "max_col_width",
                "null_value",
                "render_mode",
            ],
            0,
            0,
        )?;
        // The frozen Optional wrappers check Python types before entering Print.
        for value in fields[..3].iter().flatten().filter(|v| !v.is_none()) {
            if !value.is_instance_of::<PyInt>() {
                return Err(bad()?);
            }
        }
        if let Some(value) = fields[3].as_ref().filter(|v| !v.is_none())
            && !value.is_instance_of::<PyString>()
        {
            return Err(bad()?);
        }
        let cast_error = |value: &Bound<'_, PyAny>| -> PyResult<PyErr> {
            let class = value.get_type().repr()?.extract::<String>()?;
            Ok(pyo3::exceptions::PyRuntimeError::new_err(format!(
                "Unable to cast Python instance of type {class} to C++ type '?' (#define PYBIND11_DETAILED_ERROR_MESSAGES or compile in debug mode for details)"
            )))
        };
        let index = |value: &Option<Bound<'_, PyAny>>| -> PyResult<Option<usize>> {
            let Some(value) = value.as_ref().filter(|v| !v.is_none()) else {
                return Ok(None);
            };
            value
                .extract::<usize>()
                .map(Some)
                .map_err(|_| cast_error(value).unwrap_or_else(|e| e))
        };
        let max_width = index(&fields[0])?;
        let max_rows = index(&fields[1])?;
        let max_col_width = index(&fields[2])?;
        let null_value = fields[3]
            .as_ref()
            .filter(|v| !v.is_none())
            .map(|value| {
                value
                    .extract::<String>()
                    .map_err(|_| cast_error(value).unwrap_or_else(|e| e))
            })
            .transpose()?;
        let render_mode = fields[4].as_ref().filter(|v| !v.is_none());
        let invalidate = max_width.is_some()
            || max_rows.is_some()
            || max_col_width.is_some()
            || null_value.is_some()
            || render_mode.is_some();
        let mut config = BoxRendererConfig {
            limit: 10000,
            ..Default::default()
        };
        if let Some(width) = max_width {
            config.max_width = width;
        }
        if let Some(rows) = max_rows {
            config.max_rows = rows;
        }
        if let Some(width) = max_col_width {
            config.max_col_width = width;
        }
        if let Some(null) = null_value {
            config.null_value = null.into();
        }
        if let Some(mode) = render_mode.filter(|v| !v.is_none()) {
            let number = mode.extract::<i64>().ok();
            let text = mode.extract::<String>().ok().map(|v| v.to_ascii_lowercase());
            config.render_mode = match (number, text.as_deref()) {
                (Some(0), _) | (_, Some("rows" | "")) => BoxRenderMode::Rows,
                (Some(1), _) | (_, Some("columns")) => BoxRenderMode::Columns,
                _ => {
                    if let Some(text) = text {
                        return Err(error::native(
                            py,
                            &roastedduck_function::logging::enum_error(
                                &text,
                                "RenderMode",
                                &["ROWS", "COLUMNS"],
                            ),
                        ));
                    }
                    if number.is_some() {
                        return Err(error::native(
                            py,
                            &Error::invalid_input("Unrecognized type for 'render_mode'"),
                        ));
                    }
                    return Err(error::native(
                        py,
                        &Error::invalid_input(
                            "'render_mode' accepts either a string, RenderMode or int value",
                        ),
                    ));
                }
            };
        }
        let text = slf.borrow().render(py, config, invalidate)?;
        py.import("builtins")?.call_method1("print", (text,))?;
        Ok(())
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self)")]
    fn execute<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Bound<'py, Self>> {
        let py = slf.py();
        if !args.is_empty() || kwargs.is_some_and(|kw| !kw.is_empty()) {
            return Err(crate::error::arguments(
                py,
                "execute",
                "(self: _duckdb.DuckDBPyRelation) -> _duckdb.DuckDBPyRelation",
                Some(slf.as_any()),
                args,
                kwargs,
            )?);
        }
        let this = slf.get();
        py.detach(|| {
            let state = lock(&this.state);
            let mut connection = state.relation_connection()?;
            let mut cursor = lock(&this.cursor);
            Self::clear(&mut connection, &mut cursor);
            cursor.closed = false;
            this.ensure(&mut connection, &mut cursor, false)
        })
        .map_err(|e| error::native(py, &e))?;
        Ok(slf)
    }
    fn fetchone(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (row, zone, checkpoint) = py
            .detach(|| self.next_row(true))
            .map_err(|e| error::native(py, &e))?;
        match row {
            Some(row) => self.row_to_python(py, &row, &zone, checkpoint),
            None => Ok(py.None()),
        }
    }
    #[pyo3(signature=(size=1))]
    fn fetchmany(&self, py: Python<'_>, size: usize) -> PyResult<Py<PyAny>> {
        self.rows(py, Some(size))
    }
    fn fetchall(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.rows(py, None)
    }
    #[pyo3(signature=(vectors_per_chunk=1,*,date_as_object=false))]
    fn fetch_df_chunk(
        &self,
        py: Python<'_>,
        vectors_per_chunk: i64,
        date_as_object: bool,
    ) -> PyResult<Py<PyAny>> {
        let count = usize::try_from(vectors_per_chunk).map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err("fetch_df_chunk(): incompatible function arguments")
        })?;
        self.fetch_host_limit(py, true, date_as_object, Some(count))
    }
    fn fetchnumpy(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.fetch_host(py, false, false)
    }
    fn torch(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        crate::host::tensors(py, self.fetch_host(py, false, false)?, "torch")
    }
    fn tf(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        crate::host::tensors(py, self.fetch_host(py, false, false)?, "tensorflow")
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, batch_size=1000000, *, lazy=False)")]
    fn pl(
        slf: Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let (batch_size, lazy) = crate::polars::arguments(slf.as_any(), args, kwargs, true)?;
        if !lazy {
            return crate::polars::from_arrow(py, slf.get().fetch_arrow_named(py, batch_size, true)?);
        }
        let relation = slf.get();
        let properties = py
            .detach(|| {
                let state = lock(&relation.state);
                state
                    .relation_connection()
                    .map(|connection| connection.client_properties())
            })
            .map_err(|e| error::native(py, &e))?;
        let metadata = crate::host::Metadata {
            names: crate::host::names(&slf.get().names),
            types: slf.get().types.clone(),
            timezone: properties.time_zone.clone(),
            consumed: false,
            materialized: false,
            materialized_empty: false,
            arrow: properties,
        };
        let empty = crate::host::arrow(py, &metadata, 1_000_000, || Ok(None))?;
        let schema = py
            .import("polars")?
            .getattr("DataFrame")?
            .call1((empty,))?
            .getattr("schema")?;
        Ok(py
            .import("duckdb.polars_io")?
            .getattr("duckdb_source")?
            .call1((slf, schema))?
            .unbind())
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, batch_size=1000000)")]
    fn to_arrow_reader(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let batch_size = crate::reader::batch_arguments(slf.as_any(), args, kwargs, "to_arrow_reader", true)?;
        slf.get().arrow_reader(py, batch_size, false)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, batch_size=1000000)")]
    fn fetch_arrow_reader(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let batch_size =
            crate::reader::batch_arguments(slf.as_any(), args, kwargs, "fetch_arrow_reader", true)?;
        error::deprecated(py, "fetch_arrow_reader", "to_arrow_reader")?;
        slf.get().arrow_reader(py, batch_size, false)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, batch_size=1000000)")]
    fn arrow(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let batch_size = crate::reader::batch_arguments(slf.as_any(), args, kwargs, "arrow", true)?;
        slf.get().arrow_reader(py, batch_size, false)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, rows_per_batch=1000000)")]
    fn fetch_record_batch(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let rows_per_batch =
            crate::reader::batch_arguments(slf.as_any(), args, kwargs, "fetch_record_batch", true)?;
        py.import("warnings")?.call_method1(
            "warn",
            (
                "fetch_record_batch() is deprecated, use to_arrow_reader() instead.",
                py.get_type::<pyo3::exceptions::PyDeprecationWarning>(),
            ),
        )?;
        slf.get().arrow_reader(py, rows_per_batch, false)
    }
    #[pyo3(signature=(requested_schema=None))]
    fn __arrow_c_stream__(
        &self,
        py: Python<'_>,
        requested_schema: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let _ = requested_schema;
        self.arrow_reader(py, 1_000_000, true)
    }
    #[pyo3(signature=(batch_size=1_000_000))]
    fn to_arrow_table(&self, py: Python<'_>, batch_size: usize) -> PyResult<Py<PyAny>> {
        self.fetch_arrow(py, batch_size)
    }
    #[pyo3(signature=(batch_size=1_000_000))]
    fn fetch_arrow_table(&self, py: Python<'_>, batch_size: usize) -> PyResult<Py<PyAny>> {
        error::deprecated(py, "fetch_arrow_table", "to_arrow_table")?;
        self.fetch_arrow(py, batch_size)
    }
    #[pyo3(signature=(*,date_as_object=false))]
    fn fetchdf(&self, py: Python<'_>, date_as_object: bool) -> PyResult<Py<PyAny>> {
        self.fetch_host(py, true, date_as_object)
    }
    #[pyo3(signature=(*,date_as_object=false))]
    fn fetch_df(&self, py: Python<'_>, date_as_object: bool) -> PyResult<Py<PyAny>> {
        self.fetch_host(py, true, date_as_object)
    }
    #[pyo3(signature=(*,date_as_object=false))]
    fn df(&self, py: Python<'_>, date_as_object: bool) -> PyResult<Py<PyAny>> {
        self.fetch_host(py, true, date_as_object)
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let state = lock(&self.state);
            let mut connection = state.relation_connection()?;
            let mut cursor = lock(&self.cursor);
            if !cursor.executed && cursor.result.is_none() {
                self.ensure(&mut connection, &mut cursor, false)?;
            }
            if cursor.result.is_some() {
                Self::clear(&mut connection, &mut cursor);
                cursor.closed = true;
            }
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))
    }
    fn filter(&self, py: Python<'_>, filter_expr: &Bound<'_, PyAny>) -> PyResult<Self> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        if let Ok(sql) = filter_expr.extract::<String>() {
            return self.derive(
                py,
                &byte_format!("SELECT * FROM __relation WHERE {}", sql.as_bytes()),
                "FILTER_RELATION",
            );
        }
        let expression = filter_expr
            .extract::<PyRef<'_, crate::expression::PyExpression>>()
            .map_err(|_| {
                error::native(
                    py,
                    &Error::invalid_input(
                        "Please provide either a string or a DuckDBPyExpression object to 'filter'",
                    ),
                )
            })?;
        let mut source = parse(b"SELECT * FROM __relation").map_err(|e| error::native(py, &e))?;
        if let QueryNodeKind::Select(node) = &mut source.node.kind {
            node.where_clause = Some(Box::new(expression.native.clone()));
        }
        self.derive_select(py, source, "FILTER_RELATION")
    }
    #[pyo3(signature=(*args,groups=""))]
    fn project(&self, py: Python<'_>, args: &Bound<'_, PyTuple>, groups: &str) -> PyResult<Option<Self>> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        if args.is_empty() {
            return Ok(None);
        }
        let mut source = parse(b"SELECT * FROM __relation").map_err(|e| error::native(py, &e))?;
        let expressions = if args.len() == 1 && args.get_item(0)?.is_instance_of::<PyString>() {
            crate::expression::parse_list(py, &args.get_item(0)?.extract::<String>()?)?
        } else {
            args.iter()
                .map(|value| crate::expression::PyExpression::from_object(&value).map(|e| e.native))
                .collect::<PyResult<Vec<_>>>()?
        };
        let mut kind = "PROJECTION_RELATION";
        if !(groups.is_empty() || args.len() == 1 && args.get_item(0)?.is_instance_of::<PyString>()) {
            source = parse(&byte_format!(
                "SELECT * FROM __relation GROUP BY {}",
                groups.as_bytes()
            ))
            .map_err(|e| error::native(py, &e))?;
            kind = "AGGREGATE_RELATION";
        }
        if let QueryNodeKind::Select(node) = &mut source.node.kind {
            node.select_list = expressions;
        }
        self.derive_select(py, source, kind).map(Some)
    }
    #[pyo3(signature=(*args,groups=""))]
    fn select(&self, py: Python<'_>, args: &Bound<'_, PyTuple>, groups: &str) -> PyResult<Option<Self>> {
        self.project(py, args, groups)
    }
    #[pyo3(signature=(*args))]
    fn sort(&self, py: Python<'_>, args: &Bound<'_, PyTuple>) -> PyResult<Self> {
        let mut source = parse(b"SELECT * FROM __relation ORDER BY 1").map_err(|e| error::native(py, &e))?;
        if let Some(roastedduck_ast::ResultModifier::Order(modifier)) = source.node.modifiers.first_mut() {
            modifier.orders = args
                .iter()
                .map(|value| {
                    let expression = crate::expression::PyExpression::from_object(&value)?;
                    Ok(roastedduck_ast::OrderByNode {
                        order_type: expression.order,
                        null_order: expression.null_order,
                        expression: Box::new(expression.native),
                    })
                })
                .collect::<PyResult<Vec<_>>>()?;
        }
        self.derive_select(py, source, "ORDER_RELATION")
    }
    fn order(&self, py: Python<'_>, order_expr: &str) -> PyResult<Self> {
        self.derive(
            py,
            &byte_format!("SELECT * FROM __relation ORDER BY {}", order_expr.as_bytes()),
            "ORDER_RELATION",
        )
    }
    #[pyo3(signature=(n,offset=0))]
    fn limit(&self, py: Python<'_>, n: i64, offset: i64) -> PyResult<Self> {
        let mut template = b"SELECT * FROM __relation".to_vec();
        if n >= 0 {
            template.extend_from_slice(&byte_format!(" LIMIT {n}"));
        }
        if offset > 0 {
            template.extend_from_slice(&byte_format!(" OFFSET {offset}"));
        }
        self.derive(py, &template, "LIMIT_RELATION")
    }
    fn distinct(&self, py: Python<'_>) -> PyResult<Self> {
        self.derive(py, b"SELECT DISTINCT * FROM __relation", "DISTINCT_RELATION")
    }
    #[pyo3(signature=(other_rel,condition,how="inner"))]
    fn join(
        &self,
        py: Python<'_>,
        other_rel: Option<&Self>,
        condition: &Bound<'_, PyAny>,
        how: &str,
    ) -> PyResult<Self> {
        let other_rel = other_rel.ok_or_else(|| {
            error::InvalidInputException::new_err("Invalid Input Error: No relation provided for join")
        })?;
        let join_type = match how.trim().to_ascii_lowercase().as_str() {
            "left" => "LEFT",
            "right" => "RIGHT",
            "outer" => "FULL OUTER",
            "semi" => "SEMI",
            "inner" => "INNER",
            "anti" => "ANTI",
            _ => {
                return Err(error::native(
                    py,
                    &Error::invalid_input(format!(
                        "Unsupported join type {how}, try one of: 'left', 'right', 'outer', 'semi', 'inner', 'anti'"
                    )),
                ));
            }
        };
        self.assert_open(py)?;
        other_rel.assert_open(py)?;
        if self.alias.eq_ignore_ascii_case(&other_rel.alias) {
            return Err(error::native(
                py,
                &Error::invalid_input(
                    "Both relations have the same alias, please change the alias of one or both relations using 'rel = rel.set_alias(<new alias>)'",
                ),
            ));
        }
        let mut select =
            parse(b"SELECT * FROM __left JOIN __right ON true").map_err(|e| error::native(py, &e))?;
        let QueryNodeKind::Select(node) = &mut select.node.kind else {
            return Err(error::InternalException::new_err("Expected join template"));
        };
        let TableRefKind::Join(join) = &mut node.from_table.kind else {
            return Err(error::InternalException::new_err("Expected join reference"));
        };
        join.join_type = match join_type {
            "LEFT" => JoinType::Left,
            "RIGHT" => JoinType::Right,
            "FULL OUTER" => JoinType::Outer,
            "SEMI" => JoinType::Semi,
            "ANTI" => JoinType::Anti,
            _ => JoinType::Inner,
        };
        join.condition = None;
        if condition.is_instance_of::<PyString>() {
            let expressions = parse(&byte_format!(
                "SELECT {}",
                condition.extract::<String>()?.as_bytes()
            ))
            .map_err(|e| error::native(py, &e))?;
            let QueryNodeKind::Select(mut expressions) = expressions.node.kind else {
                return Err(error::InternalException::new_err("Expected join expressions"));
            };
            if expressions.select_list.len() > 1
                || matches!(
                    expressions.select_list.first().map(|e| &e.kind),
                    Some(ParsedExpressionKind::ColumnRef(_))
                )
            {
                for expression in expressions.select_list {
                    let ParsedExpressionKind::ColumnRef(column) = expression.kind else {
                        return Err(error::native(
                            py,
                            &Error::parser("Expected a single expression as join condition"),
                        ));
                    };
                    if column.column_names.len() != 1 {
                        return Err(error::native(
                            py,
                            &Error::parser("Expected unqualified column for column in USING clause"),
                        ));
                    }
                    join.using_columns.push(column.column_names[0].clone());
                }
            } else {
                join.condition = expressions.select_list.pop().map(Box::new);
            }
        } else if let Ok(expression) = condition.extract::<PyRef<'_, crate::expression::PyExpression>>() {
            join.condition = Some(Box::new(expression.native.clone()));
        } else if condition.is_instance_of::<PyList>() || condition.is_instance_of::<PyTuple>() {
            for column in condition.try_iter()? {
                let column = column?;
                if !column.is_instance_of::<PyString>() {
                    return Err(error::native(
                        py,
                        &Error::invalid_input(format!(
                            "Using clause should be a list of strings, not {}",
                            column.get_type().str()?
                        )),
                    ));
                }
                join.using_columns.push(column.extract::<String>()?.into());
            }
            if join.using_columns.is_empty() {
                return Err(error::native(
                    py,
                    &Error::invalid_input(
                        "Please provide at least one string in the condition to create a USING clause",
                    ),
                ));
            }
        } else {
            return Err(error::native(
                py,
                &Error::invalid_input(
                    "Please provide condition as an expression either in string form or as an Expression object",
                ),
            ));
        }
        *join.left = self.table_ref();
        *join.right = other_rel.table_ref();
        if !Arc::ptr_eq(&self.state, &other_rel.state) {
            return Err(error::native(
                py,
                &Error::invalid_input("Cannot combine LEFT and RIGHT relations of different connections!"),
            ));
        }
        Self::bind(
            py,
            self.owner.clone_ref(py),
            Arc::clone(&self.state),
            select,
            next_alias(),
            "JOIN_RELATION",
        )
    }
    fn cross(&self, py: Python<'_>, other_rel: &Self) -> PyResult<Self> {
        self.assert_open(py)?;
        other_rel.assert_open(py)?;
        if !Arc::ptr_eq(&self.state, &other_rel.state) {
            return Err(error::native(
                py,
                &Error::invalid_input("Cannot combine LEFT and RIGHT relations of different connections!"),
            ));
        }
        let mut select =
            parse(b"SELECT * FROM __left CROSS JOIN __right").map_err(|e| error::native(py, &e))?;
        let QueryNodeKind::Select(node) = &mut select.node.kind else {
            return Err(error::InternalException::new_err("Expected SELECT"));
        };
        let TableRefKind::Join(join) = &mut node.from_table.kind else {
            return Err(error::InternalException::new_err("Expected join"));
        };
        join.ref_type = JoinRefType::Cross;
        *join.left = self.table_ref();
        *join.right = other_rel.table_ref();
        Self::bind(
            py,
            self.owner.clone_ref(py),
            Arc::clone(&self.state),
            select,
            next_alias(),
            "CROSS_PRODUCT_RELATION",
        )
    }
    fn create(&self, py: Python<'_>, table_name: &str) -> PyResult<()> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        let [_, schema, name] = qualified_name(py, table_name)?;
        let target = if schema.is_empty() {
            quoted(&name)
        } else {
            byte_format!("{}.{}", quoted(&schema), quoted(&name))
        };
        let sql = byte_format!("CREATE TABLE {target} AS SELECT 42");
        let mut statement = roastedduck_parser::parse_bytes(&sql)
            .map_err(|e| error::native(py, &e))?
            .pop()
            .ok_or_else(|| error::InternalException::new_err("Expected create statement"))?;
        let SqlStatementKind::Create(info) = &mut statement.kind else {
            return Err(error::InternalException::new_err("Expected create info"));
        };
        let CreateInfoKind::Table(info) = &mut info.kind else {
            return Err(error::InternalException::new_err("Expected table info"));
        };
        info.query = Some(Box::new(self.source.clone()));
        py.detach(|| {
            let state = lock(&self.state);
            state
                .relation_connection()?
                .execute_python_statement(sql, statement)
                .map(|_| ())
        })
        .map_err(|e| error::native(py, &e))
    }
    #[pyo3(signature=(r#set,*,condition=None))]
    fn update(
        &self,
        py: Python<'_>,
        r#set: &Bound<'_, PyAny>,
        condition: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        let condition = condition
            .filter(|v| !v.is_none())
            .map(|v| {
                crate::expression::PyExpression::from_object(v)
                    .map(|e| Box::new(e.native))
                    .map_err(|_| {
                        error::native(
                            py,
                            &Error::invalid_input("Please provide an Expression to 'condition'"),
                        )
                    })
            })
            .transpose()?;
        if !(r#set.is_instance_of::<PyDict>() || r#set.hasattr("keys")? && r#set.hasattr("__getitem__")?) {
            return Err(error::native(
                py,
                &Error::invalid_input("Please provide 'set' as a dictionary of column name to Expression"),
            ));
        }
        let dictionary = py.import("builtins")?.getattr("dict")?.call1((r#set,))?;
        let dictionary = dictionary.cast::<PyDict>()?;
        if dictionary.is_empty() {
            return Err(error::native(
                py,
                &Error::invalid_input("Please provide at least one set expression"),
            ));
        }
        let mut columns = Vec::new();
        let mut expressions = Vec::new();
        for (name, value) in dictionary.iter() {
            if !name.is_instance_of::<PyString>() {
                return Err(error::native(
                    py,
                    &Error::invalid_input("Please provide the column name as the key of the dictionary"),
                ));
            }
            let expression = match crate::expression::PyExpression::from_object(&value) {
                Ok(expression) => expression.native,
                Err(_) => {
                    return Err(error::native(
                        py,
                        &Error::invalid_input(format!(
                            "Please provide an object of type Expression as the value, not {}",
                            value.get_type().str()?
                        )),
                    ));
                }
            };
            columns.push(name.extract::<String>()?.into());
            expressions.push(expression);
        }
        if self.kind != "TABLE_RELATION" {
            return Err(error::native(
                py,
                &Error::invalid_input("UPDATE can only be used on base tables!"),
            ));
        }
        let QueryNodeKind::Select(node) = &self.source.node.kind else {
            return Err(error::InternalException::new_err("Expected table relation"));
        };
        let mut statement = SqlStatement::new(SqlStatementKind::Update(Box::new(
            roastedduck_ast::UpdateStatement {
                table: node.from_table.clone(),
                from_table: None,
                set_info: Box::new(roastedduck_ast::UpdateSetInfo {
                    condition,
                    columns,
                    expressions,
                }),
                returning_list: Vec::new(),
                cte_map: roastedduck_ast::CommonTableExpressionMap::default(),
                prioritize_table_when_binding: false,
            },
        )));
        py.detach(|| {
            let state = lock(&self.state);
            let mut connection = state.relation_connection()?;
            connection.relation_columns(&mut statement)?;
            // TableRelation::Update discards its execution result in the frozen client.
            // Bind errors propagate; runtime failures still roll back/abort natively.
            let _execution = connection.execute_python_statement([], statement);
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))
    }
    fn insert(&self, py: Python<'_>, values: &Bound<'_, PyAny>) -> PyResult<()> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        if self.kind != "TABLE_RELATION" {
            return Err(error::native(
                py,
                &Error::invalid_input("'DuckDBPyRelation.insert' can only be used on a table relation"),
            ));
        }
        let QueryNodeKind::Select(node) = &self.source.node.kind else {
            return Err(error::InternalException::new_err("Expected table relation"));
        };
        let TableRefKind::BaseTable(table) = &node.from_table.kind else {
            return Err(error::InternalException::new_err("Expected base table"));
        };
        let _length = values.len()?;
        let row = values
            .try_iter()?
            .map(|v| conversion::from_python(&v?).map(roastedduck_parser::build::constant))
            .collect::<PyResult<Vec<_>>>()?;
        let mut source = roastedduck_parser::build::select_node();
        source.select_list.push(roastedduck_parser::build::star());
        *source.from_table = roastedduck_parser::build::table_ref(TableRefKind::ExpressionList(Box::new(
            roastedduck_ast::ExpressionListRef {
                expected_names: Vec::new(),
                expected_types: Vec::new(),
                values: vec![row],
            },
        )));
        source.from_table.alias = "values".into();
        let source = roastedduck_parser::build::select_statement(roastedduck_parser::build::query_node(
            QueryNodeKind::Select(Box::new(source)),
        ));
        let sql = byte_format!("INSERT INTO {} SELECT 42", quoted(&table.table_name));
        let mut statement = roastedduck_parser::parse_bytes(&sql)
            .map_err(|e| error::native(py, &e))?
            .pop()
            .ok_or_else(|| error::InternalException::new_err("Expected insert statement"))?;
        if let SqlStatementKind::Insert(info) = &mut statement.kind {
            info.catalog = table.catalog_name.clone();
            info.schema = table.schema_name.clone();
            info.table = table.table_name.clone();
            info.select_statement = Some(Box::new(source));
        }
        py.detach(|| {
            let state = lock(&self.state);
            let mut connection = state.relation_connection()?;
            if let SqlStatementKind::Insert(info) = &statement.kind
                && let Some(source) = &info.select_statement
            {
                let mut source = SqlStatement::new(SqlStatementKind::Select((**source).clone()));
                connection.relation_columns(&mut source)?;
            }
            execute_insert(&mut connection, &sql, statement, &table.table_name)
        })
        .map_err(|e| error::native(py, &e))
    }
    fn to_table(&self, py: Python<'_>, table_name: &str) -> PyResult<()> {
        self.create(py, table_name)
    }
    fn insert_into(&self, py: Python<'_>, table_name: &str) -> PyResult<()> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        let [catalog, schema, name] = qualified_name(py, table_name)?;
        let sql = byte_format!("INSERT INTO {} SELECT 42", quoted(&name));
        let mut statement = roastedduck_parser::parse_bytes(&sql)
            .map_err(|e| error::native(py, &e))?
            .pop()
            .ok_or_else(|| error::InternalException::new_err("Expected insert statement"))?;
        let SqlStatementKind::Insert(info) = &mut statement.kind else {
            return Err(error::InternalException::new_err("Expected insert info"));
        };
        info.catalog = catalog;
        info.schema = schema;
        info.table = name;
        info.select_statement = Some(Box::new(self.source.clone()));
        py.detach(|| {
            let state = lock(&self.state);
            state
                .relation_connection()?
                .execute_python_statement(sql, statement)
                .map(|_| ())
        })
        .map_err(|e| error::native(py, &e))
    }
    #[pyo3(signature=(*,date_as_object=false))]
    fn to_df(&self, py: Python<'_>, date_as_object: bool) -> PyResult<Py<PyAny>> {
        self.fetch_host(py, true, date_as_object)
    }
    #[pyo3(signature=(view_name,replace=true))]
    fn create_view(&self, py: Python<'_>, view_name: &str, replace: bool) -> PyResult<Self> {
        py.detach(|| {
            let state = lock(&self.state);
            crate::input::create_view(
                &mut *state.relation_connection()?,
                self.source.clone(),
                view_name,
                replace,
                false,
            )
        })
        .map_err(|e| error::native(py, &e))?;
        Self::bind(
            py,
            self.owner.clone_ref(py),
            Arc::clone(&self.state),
            self.source.clone(),
            self.alias.clone(),
            self.kind,
        )
    }
    #[pyo3(signature=(view_name,replace=true))]
    fn to_view(&self, py: Python<'_>, view_name: &str, replace: bool) -> PyResult<Self> {
        self.create_view(py, view_name, replace)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, virtual_table_name, sql_query)")]
    fn query(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Option<Self>> {
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyRelation, virtual_table_name: str, sql_query: str) -> _duckdb.DuckDBPyRelation";
        let bad = || crate::error::arguments(py, "query", signature, Some(slf.as_any()), args, kwargs);
        if args.len() > 2 {
            return Err(bad()?);
        }
        if let Some(kwargs) = kwargs {
            for (key, _) in kwargs.iter() {
                if !["virtual_table_name", "sql_query"].contains(&key.extract::<String>()?.as_str()) {
                    return Err(bad()?);
                }
            }
        }
        let mut parameters = Vec::new();
        for (i, name) in ["virtual_table_name", "sql_query"].iter().enumerate() {
            let kw = kwargs.map(|kw| kw.get_item(name)).transpose()?.flatten();
            if i < args.len() && kw.is_some() {
                return Err(bad()?);
            }
            let Some(value) = kw.or(if i < args.len() {
                Some(args.get_item(i)?)
            } else {
                None
            }) else {
                return Err(bad()?);
            };
            let bytes = if value.is_instance_of::<PyString>() {
                value.extract::<String>()?.into_bytes()
            } else if value.is_instance_of::<pyo3::types::PyBytes>()
                || value.is_instance_of::<pyo3::types::PyByteArray>()
            {
                let bytes = py.import("builtins")?.getattr("bytes")?.call1((value,))?;
                bytes.cast::<pyo3::types::PyBytes>()?.as_bytes().to_vec()
            } else {
                return Err(bad()?);
            };
            parameters.push(bytes);
        }
        let virtual_table_name = &parameters[0];
        let sql_query = &parameters[1];
        let this = slf.get();
        py.detach(|| {
            let state = lock(&this.state);
            crate::input::create_view(
                &mut *state.relation_connection()?,
                this.source.clone(),
                virtual_table_name,
                true,
                true,
            )
        })
        .map_err(|e| error::native(py, &e))?;
        let statements = roastedduck_parser::parse_bytes(sql_query).map_err(|e| error::native(py, &e))?;
        if statements.len() != 1 {
            return Err(error::native(
                py,
                &Error::invalid_input("'DuckDBPyRelation.query' only accepts a single statement"),
            ));
        }
        let is_select = matches!(statements[0].kind, SqlStatementKind::Select(_));
        let result = Self::from_sql(this.owner.bind(py).clone(), sql_query, "", None)?;
        Ok(if is_select { result } else { None })
    }
    fn union(&self, py: Python<'_>, other_rel: &Self) -> PyResult<Self> {
        self.set_operation(py, other_rel, SetOperationType::Union)
    }
    fn except_(&self, py: Python<'_>, other_rel: &Self) -> PyResult<Self> {
        self.set_operation(py, other_rel, SetOperationType::Except)
    }
    fn intersect(&self, py: Python<'_>, other_rel: &Self) -> PyResult<Self> {
        self.set_operation(py, other_rel, SetOperationType::Intersect)
    }
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature=(file_name,*,sep=None,na_rep=None,header=None,quotechar=None,escapechar=None,date_format=None,timestamp_format=None,quoting=None,encoding=None,compression=None,overwrite=None,per_thread_output=None,use_tmp_file=None,partition_by=None,write_partition_columns=None))]
    fn to_csv(
        &self,
        py: Python<'_>,
        file_name: &str,
        sep: Option<&Bound<'_, PyAny>>,
        na_rep: Option<&Bound<'_, PyAny>>,
        header: Option<&Bound<'_, PyAny>>,
        quotechar: Option<&Bound<'_, PyAny>>,
        escapechar: Option<&Bound<'_, PyAny>>,
        date_format: Option<&Bound<'_, PyAny>>,
        timestamp_format: Option<&Bound<'_, PyAny>>,
        quoting: Option<&Bound<'_, PyAny>>,
        encoding: Option<&Bound<'_, PyAny>>,
        compression: Option<&Bound<'_, PyAny>>,
        overwrite: Option<&Bound<'_, PyAny>>,
        per_thread_output: Option<&Bound<'_, PyAny>>,
        use_tmp_file: Option<&Bound<'_, PyAny>>,
        partition_by: Option<&Bound<'_, PyAny>>,
        write_partition_columns: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let options = crate::files::write_options(
            py,
            &[
                ("sep", sep),
                ("na_rep", na_rep),
                ("header", header),
                ("quotechar", quotechar),
                ("escapechar", escapechar),
                ("date_format", date_format),
                ("timestamp_format", timestamp_format),
                ("quoting", quoting),
                ("encoding", encoding),
                ("compression", compression),
                ("overwrite", overwrite),
                ("per_thread_output", per_thread_output),
                ("use_tmp_file", use_tmp_file),
                ("partition_by", partition_by),
                ("write_partition_columns", write_partition_columns),
            ],
        )?;
        self.write_file(py, file_name, "csv", options)
    }
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature=(file_name,*,sep=None,na_rep=None,header=None,quotechar=None,escapechar=None,date_format=None,timestamp_format=None,quoting=None,encoding=None,compression=None,overwrite=None,per_thread_output=None,use_tmp_file=None,partition_by=None,write_partition_columns=None))]
    fn write_csv(
        &self,
        py: Python<'_>,
        file_name: &str,
        sep: Option<&Bound<'_, PyAny>>,
        na_rep: Option<&Bound<'_, PyAny>>,
        header: Option<&Bound<'_, PyAny>>,
        quotechar: Option<&Bound<'_, PyAny>>,
        escapechar: Option<&Bound<'_, PyAny>>,
        date_format: Option<&Bound<'_, PyAny>>,
        timestamp_format: Option<&Bound<'_, PyAny>>,
        quoting: Option<&Bound<'_, PyAny>>,
        encoding: Option<&Bound<'_, PyAny>>,
        compression: Option<&Bound<'_, PyAny>>,
        overwrite: Option<&Bound<'_, PyAny>>,
        per_thread_output: Option<&Bound<'_, PyAny>>,
        use_tmp_file: Option<&Bound<'_, PyAny>>,
        partition_by: Option<&Bound<'_, PyAny>>,
        write_partition_columns: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let options = crate::files::write_options(
            py,
            &[
                ("sep", sep),
                ("na_rep", na_rep),
                ("header", header),
                ("quotechar", quotechar),
                ("escapechar", escapechar),
                ("date_format", date_format),
                ("timestamp_format", timestamp_format),
                ("quoting", quoting),
                ("encoding", encoding),
                ("compression", compression),
                ("overwrite", overwrite),
                ("per_thread_output", per_thread_output),
                ("use_tmp_file", use_tmp_file),
                ("partition_by", partition_by),
                ("write_partition_columns", write_partition_columns),
            ],
        )?;
        self.write_file(py, file_name, "csv", options)
    }
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature=(file_name,*,compression=None,field_ids=None,row_group_size_bytes=None,row_group_size=None,overwrite=None,per_thread_output=None,use_tmp_file=None,partition_by=None,write_partition_columns=None,append=None,filename_pattern=None,file_size_bytes=None))]
    fn to_parquet(
        &self,
        py: Python<'_>,
        file_name: &str,
        compression: Option<&Bound<'_, PyAny>>,
        field_ids: Option<&Bound<'_, PyAny>>,
        row_group_size_bytes: Option<&Bound<'_, PyAny>>,
        row_group_size: Option<&Bound<'_, PyAny>>,
        overwrite: Option<&Bound<'_, PyAny>>,
        per_thread_output: Option<&Bound<'_, PyAny>>,
        use_tmp_file: Option<&Bound<'_, PyAny>>,
        partition_by: Option<&Bound<'_, PyAny>>,
        write_partition_columns: Option<&Bound<'_, PyAny>>,
        append: Option<&Bound<'_, PyAny>>,
        filename_pattern: Option<&Bound<'_, PyAny>>,
        file_size_bytes: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let options = crate::files::parquet_write_options(
            py,
            &[
                ("compression", compression),
                ("field_ids", field_ids),
                ("row_group_size_bytes", row_group_size_bytes),
                ("row_group_size", row_group_size),
                ("overwrite", overwrite),
                ("per_thread_output", per_thread_output),
                ("use_tmp_file", use_tmp_file),
                ("partition_by", partition_by),
                ("write_partition_columns", write_partition_columns),
                ("append", append),
                ("filename_pattern", filename_pattern),
                ("file_size_bytes", file_size_bytes),
            ],
        )?;
        self.write_file(py, file_name, "parquet", options)
    }
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature=(file_name,*,compression=None,field_ids=None,row_group_size_bytes=None,row_group_size=None,overwrite=None,per_thread_output=None,use_tmp_file=None,partition_by=None,write_partition_columns=None,append=None,filename_pattern=None,file_size_bytes=None))]
    fn write_parquet(
        &self,
        py: Python<'_>,
        file_name: &str,
        compression: Option<&Bound<'_, PyAny>>,
        field_ids: Option<&Bound<'_, PyAny>>,
        row_group_size_bytes: Option<&Bound<'_, PyAny>>,
        row_group_size: Option<&Bound<'_, PyAny>>,
        overwrite: Option<&Bound<'_, PyAny>>,
        per_thread_output: Option<&Bound<'_, PyAny>>,
        use_tmp_file: Option<&Bound<'_, PyAny>>,
        partition_by: Option<&Bound<'_, PyAny>>,
        write_partition_columns: Option<&Bound<'_, PyAny>>,
        append: Option<&Bound<'_, PyAny>>,
        filename_pattern: Option<&Bound<'_, PyAny>>,
        file_size_bytes: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let options = crate::files::parquet_write_options(
            py,
            &[
                ("compression", compression),
                ("field_ids", field_ids),
                ("row_group_size_bytes", row_group_size_bytes),
                ("row_group_size", row_group_size),
                ("overwrite", overwrite),
                ("per_thread_output", per_thread_output),
                ("use_tmp_file", use_tmp_file),
                ("partition_by", partition_by),
                ("write_partition_columns", write_partition_columns),
                ("append", append),
                ("filename_pattern", filename_pattern),
                ("file_size_bytes", file_size_bytes),
            ],
        )?;
        self.write_file(py, file_name, "parquet", options)
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn any_value(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "any_value",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn avg(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "avg",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn mean(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "avg",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn bit_and(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "bit_and",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn bit_or(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "bit_or",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn bit_xor(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "bit_xor",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn bool_and(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "bool_and",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn bool_or(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "bool_or",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn count(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "count",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn favg(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "favg",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn fsum(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "fsum",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn histogram(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "histogram",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn list(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "list",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn max(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "max",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn min(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "min",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn product(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "product",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn sum(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "sum",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn median(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "median",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn mode(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "mode",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn stddev_pop(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "stddev_pop",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn stddev_samp(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "stddev_samp",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn stddev(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "stddev_samp",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn std(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "stddev_samp",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn var_pop(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "var_pop",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn var_samp(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "var_samp",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn variance(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "var_samp",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",window_spec="",projected_columns=""))]
    fn var(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "var_samp",
            expression,
            "",
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups="",projected_columns=""))]
    fn first(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(py, "first", expression, "", groups, "", projected_columns, false)
    }
    #[pyo3(signature=(expression,groups="",projected_columns=""))]
    fn last(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(py, "last", expression, "", groups, "", projected_columns, false)
    }
    #[pyo3(signature=(expression,groups="",projected_columns=""))]
    fn geomean(
        &self,
        py: Python<'_>,
        expression: &str,
        groups: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "geomean",
            expression,
            "",
            groups,
            "",
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(arg_column,value_column,groups="",window_spec="",projected_columns=""))]
    fn arg_max(
        &self,
        py: Python<'_>,
        arg_column: &str,
        value_column: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "arg_max",
            arg_column,
            value_column,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(arg_column,value_column,groups="",window_spec="",projected_columns=""))]
    fn arg_min(
        &self,
        py: Python<'_>,
        arg_column: &str,
        value_column: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            "arg_min",
            arg_column,
            value_column,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,q=pyo3::Python::attach(|py|pyo3::types::PyFloat::new(py,0.5).into_any().unbind()),groups="",window_spec="",projected_columns=""),text_signature="($self, expression, q=0.5, groups='', window_spec='', projected_columns='')")]
    fn quantile(
        &self,
        py: Python<'_>,
        expression: &str,
        q: Py<PyAny>,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let parameters = crate::aggregation::quantile(py, &q.into_bound(py))?;
        self.aggregation(
            py,
            "quantile_disc",
            expression,
            &parameters,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,q=pyo3::Python::attach(|py|pyo3::types::PyFloat::new(py,0.5).into_any().unbind()),groups="",window_spec="",projected_columns=""),text_signature="($self, expression, q=0.5, groups='', window_spec='', projected_columns='')")]
    fn quantile_cont(
        &self,
        py: Python<'_>,
        expression: &str,
        q: Py<PyAny>,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let parameters = crate::aggregation::quantile(py, &q.into_bound(py))?;
        self.aggregation(
            py,
            "quantile_cont",
            expression,
            &parameters,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,q=pyo3::Python::attach(|py|pyo3::types::PyFloat::new(py,0.5).into_any().unbind()),groups="",window_spec="",projected_columns=""),text_signature="($self, expression, q=0.5, groups='', window_spec='', projected_columns='')")]
    fn quantile_disc(
        &self,
        py: Python<'_>,
        expression: &str,
        q: Py<PyAny>,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let parameters = crate::aggregation::quantile(py, &q.into_bound(py))?;
        self.aggregation(
            py,
            "quantile_disc",
            expression,
            &parameters,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,min=None,max=None,groups="",window_spec="",projected_columns=""))]
    #[allow(clippy::too_many_arguments)]
    fn bitstring_agg(
        &self,
        py: Python<'_>,
        expression: &str,
        min: Option<&Bound<'_, PyAny>>,
        max: Option<&Bound<'_, PyAny>>,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let parameters = crate::aggregation::bitstring(py, min, max)?;
        self.aggregation(
            py,
            "bitstring_agg",
            expression,
            &parameters,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,sep=",",groups="",window_spec="",projected_columns=""))]
    fn string_agg(
        &self,
        py: Python<'_>,
        expression: &str,
        sep: &str,
        groups: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let parameter = roastedduck_ast::keyword::write_optionally_quoted_with(
            sep,
            '\'',
            true,
            roastedduck_ast::keyword::KeywordCategory::None,
        );
        self.aggregation(
            py,
            "string_agg",
            expression,
            &parameter,
            groups,
            window_spec,
            projected_columns,
            false,
        )
    }
    #[pyo3(signature=(expression,groups=""))]
    fn value_counts(&self, py: Python<'_>, expression: &str, groups: &str) -> PyResult<Self> {
        self.aggregation(py, "count", expression, "", groups, "", expression, false)
    }
    #[pyo3(signature=(function_name,function_aggr,group_expr="",function_parameter="",projected_columns=""))]
    fn apply(
        &self,
        py: Python<'_>,
        function_name: &str,
        function_aggr: &str,
        group_expr: &str,
        function_parameter: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.aggregation(
            py,
            function_name,
            function_aggr,
            function_parameter,
            group_expr,
            "",
            projected_columns,
            false,
        )
    }
    fn unique(&self, py: Python<'_>, unique_aggr: &str) -> PyResult<Self> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        self.derive(
            py,
            &byte_format!("SELECT {} FROM __relation", unique_aggr.as_bytes()),
            "PROJECTION_RELATION",
        )?
        .distinct(py)
    }
    #[pyo3(signature=(window_spec,projected_columns=""))]
    fn row_number(&self, py: Python<'_>, window_spec: &str, projected_columns: &str) -> PyResult<Self> {
        self.window_function(py, "row_number", "", "*", window_spec, false, projected_columns)
    }
    #[pyo3(signature=(window_spec,projected_columns=""))]
    fn rank(&self, py: Python<'_>, window_spec: &str, projected_columns: &str) -> PyResult<Self> {
        self.window_function(py, "rank", "", "*", window_spec, false, projected_columns)
    }
    #[pyo3(signature=(window_spec,projected_columns=""))]
    fn dense_rank(&self, py: Python<'_>, window_spec: &str, projected_columns: &str) -> PyResult<Self> {
        self.window_function(py, "dense_rank", "", "*", window_spec, false, projected_columns)
    }
    #[pyo3(signature=(window_spec,projected_columns=""))]
    fn rank_dense(&self, py: Python<'_>, window_spec: &str, projected_columns: &str) -> PyResult<Self> {
        self.window_function(py, "dense_rank", "", "*", window_spec, false, projected_columns)
    }
    #[pyo3(signature=(window_spec,projected_columns=""))]
    fn percent_rank(&self, py: Python<'_>, window_spec: &str, projected_columns: &str) -> PyResult<Self> {
        self.window_function(py, "percent_rank", "", "*", window_spec, false, projected_columns)
    }
    #[pyo3(signature=(window_spec,projected_columns=""))]
    fn cume_dist(&self, py: Python<'_>, window_spec: &str, projected_columns: &str) -> PyResult<Self> {
        self.window_function(py, "cume_dist", "", "*", window_spec, false, projected_columns)
    }
    #[pyo3(signature=(expression,window_spec="",projected_columns=""))]
    fn first_value(
        &self,
        py: Python<'_>,
        expression: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.window_function(
            py,
            "first_value",
            "",
            expression,
            window_spec,
            false,
            projected_columns,
        )
    }
    #[pyo3(signature=(expression,window_spec="",projected_columns=""))]
    fn last_value(
        &self,
        py: Python<'_>,
        expression: &str,
        window_spec: &str,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.window_function(
            py,
            "last_value",
            "",
            expression,
            window_spec,
            false,
            projected_columns,
        )
    }
    #[pyo3(signature=(expression,window_spec,offset=1,default_value="NULL",ignore_nulls=false,projected_columns=""))]
    #[allow(clippy::too_many_arguments)]
    fn lag(
        &self,
        py: Python<'_>,
        expression: &str,
        window_spec: &str,
        offset: i32,
        default_value: &str,
        ignore_nulls: bool,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let mut parameters = if offset == 0 {
            String::new()
        } else {
            offset.to_string()
        };
        if !default_value.is_empty() {
            parameters.push(',');
            parameters.push_str(default_value);
        }
        self.window_function(
            py,
            "lag",
            &parameters,
            expression,
            window_spec,
            ignore_nulls,
            projected_columns,
        )
    }
    #[pyo3(signature=(expression,window_spec,offset=1,default_value="NULL",ignore_nulls=false,projected_columns=""))]
    #[allow(clippy::too_many_arguments)]
    fn lead(
        &self,
        py: Python<'_>,
        expression: &str,
        window_spec: &str,
        offset: i32,
        default_value: &str,
        ignore_nulls: bool,
        projected_columns: &str,
    ) -> PyResult<Self> {
        let mut parameters = if offset == 0 {
            String::new()
        } else {
            offset.to_string()
        };
        if !default_value.is_empty() {
            parameters.push(',');
            parameters.push_str(default_value);
        }
        self.window_function(
            py,
            "lead",
            &parameters,
            expression,
            window_spec,
            ignore_nulls,
            projected_columns,
        )
    }
    #[pyo3(signature=(expression,window_spec,offset,ignore_nulls=false,projected_columns=""))]
    fn nth_value(
        &self,
        py: Python<'_>,
        expression: &str,
        window_spec: &str,
        offset: i32,
        ignore_nulls: bool,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.window_function(
            py,
            "nth_value",
            &offset.to_string(),
            expression,
            window_spec,
            ignore_nulls,
            projected_columns,
        )
    }
    #[pyo3(signature=(window_spec,num_buckets,projected_columns=""))]
    fn n_tile(
        &self,
        py: Python<'_>,
        window_spec: &str,
        num_buckets: i32,
        projected_columns: &str,
    ) -> PyResult<Self> {
        self.window_function(
            py,
            "ntile",
            &num_buckets.to_string(),
            "",
            window_spec,
            false,
            projected_columns,
        )
    }
    fn describe(&self, py: Python<'_>) -> PyResult<Self> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        let aggregates = [
            ("count", false),
            ("mean", true),
            ("stddev", true),
            ("min", false),
            ("max", false),
            ("median", true),
        ];
        let mut sql = b"SELECT UNNEST(['count','mean','stddev','min','max','median']) AS aggr".to_vec();
        for (name, kind) in self.names.iter().zip(&self.types) {
            sql.extend_from_slice(b", UNNEST([");
            for (i, (aggregate, numeric_only)) in aggregates.iter().enumerate() {
                if i != 0 {
                    sql.extend_from_slice(b", ");
                }
                if *numeric_only && !kind.is_numeric() {
                    sql.extend_from_slice(b"NULL");
                } else {
                    sql.extend_from_slice(&byte_format!(
                        "{}({})::{}",
                        aggregate,
                        quoted(name),
                        if kind.is_numeric() { "DOUBLE" } else { "VARCHAR" }
                    ));
                }
            }
            sql.extend_from_slice(&byte_format!("]) AS {}", quoted(name)));
        }
        sql.extend_from_slice(b" FROM __relation GROUP BY ALL");
        self.derive(py, &sql, "AGGREGATE_RELATION")
    }
    #[pyo3(signature=(aggr_expr,group_expr=""))]
    fn aggregate(&self, py: Python<'_>, aggr_expr: &Bound<'_, PyAny>, group_expr: &str) -> PyResult<Self> {
        py.detach(|| lock(&self.state).relation_connection().map(|_| ()))
            .map_err(|e| error::native(py, &e))?;
        let actual_type = aggr_expr.get_type().str()?.extract::<String>()?;
        let groups = if group_expr.is_empty() { "ALL" } else { group_expr };
        let grouping = parse(&byte_format!("SELECT 42 GROUP BY {}", groups.as_bytes()))
            .map_err(|e| error::native(py, &e))?;
        let mut source = parse(b"SELECT * FROM __relation").map_err(|e| error::native(py, &e))?;
        if let QueryNodeKind::Select(group) = grouping.node.kind
            && let QueryNodeKind::Select(node) = &mut source.node.kind
        {
            if group.group_expressions.is_empty() {
                node.aggregate_handling = AggregateHandling::ForceAggregates;
            } else {
                node.group_expressions = group.group_expressions;
                node.group_sets = group.group_sets;
                node.aggregate_handling = AggregateHandling::StandardHandling;
            }
        }
        let expressions = if let Ok(sql) = aggr_expr.extract::<String>() {
            crate::expression::parse_list(py, &sql)?
        } else {
            aggr_expr
                .try_iter()
                .map_err(|_| {
                    error::native(
                        py,
                        &Error::invalid_input(format!(
                            "Please provide either a string or list of Expression objects, not {actual_type}"
                        )),
                    )
                })?
                .map(|v| {
                    crate::expression::PyExpression::from_object(&v?)
                        .map(|e| e.native)
                        .map_err(|_| {
                            error::native(
                                py,
                                &Error::invalid_input("Please provide arguments of type Expression!"),
                            )
                        })
                })
                .collect::<PyResult<Vec<_>>>()?
        };
        if let QueryNodeKind::Select(node) = &mut source.node.kind {
            node.select_list = expressions;
        }
        self.derive_select(py, source, "AGGREGATE_RELATION")
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn set_alias<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Self> {
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyRelation, alias: str) -> _duckdb.DuckDBPyRelation";
        let values = crate::arguments::fields(
            "set_alias",
            signature,
            slf.as_any(),
            args,
            kwargs,
            &["alias"],
            1,
            1,
        )?;
        let value = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated alias"))?;
        let Some(alias) = crate::arguments::string_bytes(value)? else {
            return Err(error::arguments(
                py,
                "set_alias",
                signature,
                Some(slf.as_any()),
                args,
                kwargs,
            )?);
        };
        let this = slf.get();
        let mut result = Self::bind(
            py,
            this.owner.clone_ref(py),
            Arc::clone(&this.state),
            this.source.clone(),
            alias.into(),
            "SUBQUERY_RELATION",
        )?;
        result.join_bindings = matches!(
            this.kind,
            "FILTER_RELATION" | "ORDER_RELATION" | "LIMIT_RELATION" | "DISTINCT_RELATION"
        ) && this.join_bindings;
        Ok(result)
    }
    fn sql_query(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        conversion::text(py, &self.source.to_sql_bytes())
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, type='standard')")]
    fn explain(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let bad = || {
            error::arguments(
                py,
                "explain",
                "(self: _duckdb.DuckDBPyRelation, type: _duckdb.ExplainType = 'standard') -> str",
                Some(slf.as_any()),
                args,
                kwargs,
            )
        };
        if args.len() > 1
            || kwargs.is_some_and(|kwargs| {
                kwargs
                    .iter()
                    .any(|(key, _)| key.extract::<String>().is_ok_and(|key| key != "type"))
            })
        {
            return Err(bad()?);
        }
        let keyword = kwargs
            .map(|kwargs| kwargs.get_item("type"))
            .transpose()?
            .flatten();
        if !args.is_empty() && keyword.is_some() {
            return Err(bad()?);
        }
        let value = if args.is_empty() {
            keyword
        } else {
            Some(args.get_item(0)?)
        };
        let analyze = if let Some(value) = value {
            let Some(value) = crate::statement::explain_type(&value)? else {
                return Err(bad()?);
            };
            value == 1
        } else {
            false
        };
        let this = slf.get();
        let sql = if analyze {
            b"EXPLAIN ANALYZE SELECT 42".as_slice()
        } else {
            b"EXPLAIN SELECT 42".as_slice()
        };
        let mut statement = roastedduck_parser::parse_bytes(sql)
            .map_err(|e| error::native(py, &e))?
            .pop()
            .ok_or_else(|| error::InternalException::new_err("Expected explain statement"))?;
        let SqlStatementKind::Explain(info) = &mut statement.kind else {
            return Err(error::InternalException::new_err("Expected explain info"));
        };
        *info.statement = crate::relation::statement(this.source.clone());
        let output = py
            .detach(|| {
                let state = lock(&this.state);
                let mut connection = state.relation_connection()?;
                // ExplainRelation has no SQL query string. ANALYZE still executes
                // the native plan, then renders the native profiler under the
                // connection's configured profiling state, as the C++ client does.
                let result = connection.execute_python_statement([], statement)?;
                if analyze {
                    return connection
                        .profiling_information("query_tree")
                        .map(|text| text.to_vec());
                }
                let mut output = Vec::new();
                for chunk in result.collect_chunks()? {
                    for row in 0..chunk.len() {
                        for col in 1..chunk.column_count() {
                            if col > 1 {
                                output.push(b'\t');
                            }
                            let value = chunk.column(col).get_value(row);
                            if value.is_null() {
                                output.extend_from_slice(b"NULL");
                            } else {
                                let text = value.to_varchar_bytes()?;
                                for &byte in text.iter() {
                                    if byte == 0 {
                                        output.extend_from_slice(b"\\0");
                                    } else {
                                        output.push(byte);
                                    }
                                }
                            }
                        }
                        output.push(b'\n');
                    }
                }
                Ok::<_, Error>(output)
            })
            .map_err(|e| error::native(py, &e))?;
        conversion::text(py, &output)
    }
    #[getter]
    fn alias(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        conversion::text(py, &self.alias)
    }
    #[getter]
    fn r#type(&self) -> &'static str {
        self.kind
    }
    #[getter]
    fn columns(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyList::new(
            py,
            self.names
                .iter()
                .map(|name| conversion::text(py, name))
                .collect::<PyResult<Vec<_>>>()?,
        )?
        .unbind()
        .into_any())
    }
    #[getter]
    fn types(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyList::new(
            py,
            self.types
                .iter()
                .map(|kind| Py::new(py, PyType { native: kind.clone() }))
                .collect::<PyResult<Vec<_>>>()?,
        )?
        .unbind()
        .into_any())
    }
    #[getter]
    fn dtypes(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.types(py)
    }
    #[getter]
    fn description(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rows = PyList::empty(py);
        for (name, kind) in self.names.iter().zip(&self.types) {
            rows.append(PyTuple::new(
                py,
                [
                    conversion::text(py, name)?,
                    Py::new(py, PyType { native: kind.clone() })?.into_any(),
                    py.None(),
                    py.None(),
                    py.None(),
                    py.None(),
                    py.None(),
                ],
            )?)?;
        }
        Ok(rows.unbind().into_any())
    }
    #[getter]
    fn shape(&self, py: Python<'_>) -> PyResult<(usize, usize)> {
        Ok((self.__len__(py)?, self.names.len()))
    }
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let relation = self.derive(
            py,
            b"SELECT count(*)::UBIGINT FROM __relation",
            "AGGREGATE_RELATION",
        )?;
        let row = py
            .detach(|| relation.next_row(false))
            .map_err(|e| error::native(py, &e))?
            .0;
        row.and_then(|row| row.first().and_then(Value::as_i128))
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| error::InternalException::new_err("Invalid relation count"))
    }
    fn __contains__(&self, name: &str) -> bool {
        self.names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name.as_bytes()))
    }
    fn select_types(&self, py: Python<'_>, types: &Bound<'_, PyAny>) -> PyResult<Self> {
        let list = types.cast::<PyList>().map_err(|_| {
            error::native(
                py,
                &Error::invalid_input("'columns_by_type' expects a list containing types"),
            )
        })?;
        if list.is_empty() {
            return Err(error::native(
                py,
                &Error::invalid_input("List of types can not be empty!"),
            ));
        }
        let mut filter = Vec::new();
        for item in list.iter() {
            if let Ok(kind) = item.extract::<PyRef<'_, PyType>>() {
                filter.push(kind.native.clone());
            } else if let Ok(name) = item.extract::<String>() {
                filter.push(PyType::parse(py, &name)?.native);
            } else {
                return Err(error::native(
                    py,
                    &Error::invalid_input(format!(
                        "Can only project on objects of type DuckDBPyType or str, not '{}'",
                        item.get_type().str()?
                    )),
                ));
            }
        }
        let expressions = self
            .names
            .iter()
            .zip(&self.types)
            .filter(|(_, kind)| filter.contains(kind))
            .map(|(name, _)| roastedduck_parser::build::column_ref(vec![name.clone()]))
            .collect::<Vec<_>>();
        if expressions.is_empty() {
            return Err(error::native(
                py,
                &Error::invalid_input("None of the columns matched the provided type filter!"),
            ));
        }
        let mut source = parse(b"SELECT * FROM __relation").map_err(|e| error::native(py, &e))?;
        if let QueryNodeKind::Select(node) = &mut source.node.kind {
            node.select_list = expressions;
        }
        self.derive_select(py, source, "PROJECTION_RELATION")
    }
    fn select_dtypes(&self, py: Python<'_>, types: &Bound<'_, PyAny>) -> PyResult<Self> {
        self.select_types(py, types)
    }
    fn __getitem__(&self, py: Python<'_>, name: &str) -> PyResult<Self> {
        self.__getattr__(py, name)
    }
    fn _lookup_attribute(&self, py: Python<'_>, name: &str) -> PyResult<Self> {
        self.__getattr__(py, name)
    }
    fn __getattr__(&self, py: Python<'_>, name: &str) -> PyResult<Self> {
        let mut columns = Vec::new();
        if self.names.len() == 1
            && self.types[0].struct_children().is_some_and(|fields| {
                fields
                    .iter()
                    .any(|(field, _)| field.eq_ignore_ascii_case(name.as_bytes()))
            })
        {
            columns.push(self.names[0].clone());
            columns.push(name.into());
        } else if self
            .names
            .iter()
            .any(|field| field.eq_ignore_ascii_case(name.as_bytes()))
        {
            columns.push(name.into());
        }
        if columns.is_empty() {
            return Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                "This relation does not contain a column by the name of '{name}'"
            )));
        }
        let mut source = parse(b"SELECT * FROM __relation").map_err(|e| error::native(py, &e))?;
        if let QueryNodeKind::Select(node) = &mut source.node.kind {
            node.select_list = vec![roastedduck_parser::build::column_ref(columns)];
        }
        self.derive_select(py, source, "PROJECTION_RELATION")
    }
}
