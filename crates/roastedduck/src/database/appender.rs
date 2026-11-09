//! Appending rows to a table, or to a query that reads them (DuckDB's `BaseAppender`,
//! `Appender` and `QueryAppender` in `main/appender.cpp`, with `ClientContext::TableInfo`
//! and `ClientContext::Append`).
//!
//! Rows are gathered into chunks of `STANDARD_VECTOR_SIZE` rows; a flush runs one
//! statement over all gathered rows: `INSERT INTO table FROM __duckdb_internal_appended_data`
//! for a table, the client's own statement for a query appender. The rows reach the
//! statement as a common table expression over a `ColumnDataRef`.

use std::collections::HashMap;

use roastedduck_ast::{
    ColumnDataRef, CommonTableExpressionInfo, CteMaterialize, ParsedExpression, QueryNodeKind, SqlStatement,
    SqlStatementKind, TableRefKind,
};
use roastedduck_catalog::{CatalogType, EntryKind};
use roastedduck_common::{Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_parser::build;
use roastedduck_planner::Binder;
use roastedduck_planner::expression_binder::{ConstantBinder, ExpressionBinder};
use roastedduck_types::{ColumnDataCollection, DataChunk, LogicalType, STANDARD_VECTOR_SIZE, Value};

use super::Connection;

mod cast;

/// A column of a table as clients see it (DuckDB's `ColumnDefinition` in a
/// `TableDescription`).
#[derive(Clone, Debug)]
pub struct ColumnDescription {
    pub name: roastedduck_common::ByteString,
    pub logical_type: LogicalType,
    /// The `DEFAULT` expression, if the column has one.
    pub default_value: Option<ParsedExpression>,
    /// Whether the column is generated (`AS (expression)`).
    pub generated: bool,
}

/// A table's columns, as found by name (DuckDB's `TableDescription`): `database` and
/// `schema` are the names the table was looked up with.
#[derive(Clone, Debug)]
pub struct TableDescription {
    pub database: roastedduck_common::ByteString,
    pub schema: roastedduck_common::ByteString,
    pub table: roastedduck_common::ByteString,
    /// Whether the table's database is attached read-only.
    pub readonly: bool,
    /// Every column, generated ones included, in table order.
    pub columns: Vec<ColumnDescription>,
}

impl Connection {
    /// The description of table `table` in `schema` of `database` (an empty database
    /// name is the default one), `None` if there is no such table (DuckDB's
    /// `ClientContext::TableInfo`, run in a transaction of its own outside one).
    pub fn table_info(
        &mut self,
        database: impl AsRef<[u8]>,
        schema: impl AsRef<[u8]>,
        table: impl AsRef<[u8]>,
    ) -> Result<Option<TableDescription>> {
        let (database, schema, table) = (database.as_ref(), schema.as_ref(), table.as_ref());
        self.initial_cleanup();
        let transaction = self.transactions.current();
        let context = self.client_context(transaction);
        let found = (|| {
            let Some(lookup) = context.get_entry(database, schema, CatalogType::Table, table, true)? else {
                return Ok(None);
            };
            let EntryKind::Table(entry) = &lookup.entry.kind else {
                return Ok(None);
            };
            let catalog = if database.is_empty() {
                self.default_catalog()
            } else {
                database.into()
            };
            let readonly = self
                .database
                .instance()
                .databases
                .get(&catalog)
                .is_some_and(|attached| attached.read_only);
            let columns = entry
                .columns
                .iter()
                .map(|column| ColumnDescription {
                    name: column.name.clone(),
                    logical_type: column.logical_type.clone(),
                    default_value: column.default_value.clone(),
                    generated: column.generated.is_some(),
                })
                .collect();
            Ok(Some(TableDescription {
                database: database.into(),
                schema: schema.into(),
                table: table.into(),
                readonly,
                columns,
            }))
        })();
        let ended = self.end_client_statement(found.is_ok(), false);
        found.and_then(|found| ended.map(|()| found))
    }

    /// Runs a statement that appends rows; a failure is prefixed with
    /// `Failed to append: ` (`ClientContext::Append`).
    fn append_statement(&mut self, statement: SqlStatement) -> Result<()> {
        let query = statement.query.clone();
        self.execute_statement(&query, statement)
            .map(drop)
            .map_err(|error| {
                Error::new_bytes(
                    error.kind(),
                    roastedduck_common::byte_format!("Failed to append: {}", error.raw_message_bytes()),
                )
            })
    }

    /// The constant values of the columns' `DEFAULT` expressions, by column index
    /// (`Appender::Appender`): a column without a default has NULL; a default that is not
    /// constant (e.g. `nextval`) has none.
    fn constant_defaults(&mut self, columns: &[&ColumnDescription]) -> Result<HashMap<usize, Value>> {
        let transaction = self.transactions.current();
        let context = self.client_context(transaction);
        let defaults = (|| {
            let mut defaults = HashMap::new();
            for (index, column) in columns.iter().enumerate() {
                let Some(default) = &column.default_value else {
                    defaults.insert(index, Value::null(column.logical_type.clone()));
                    continue;
                };
                let mut binder = Binder::new(std::sync::Arc::clone(&context))?;
                let mut expression = default.clone();
                let bound = ConstantBinder::new(&mut binder, None, "DEFAULT value")
                    .with_target_type(column.logical_type.clone())
                    .bind(&mut expression, true)?;
                if !bound.is_foldable() {
                    continue;
                }
                if let Ok(value) = context.evaluator.evaluate_scalar(&bound) {
                    defaults.insert(index, value);
                }
            }
            Ok(defaults)
        })();
        let ended = self.end_client_statement(defaults.is_ok(), false);
        defaults.and_then(|defaults| ended.map(|()| defaults))
    }
}

/// Where an appender's rows go.
#[derive(Debug)]
enum AppenderTarget {
    /// A table (DuckDB's `Appender`), with the constant default of each non-generated
    /// column and the logical indexes of the columns appended to (all when empty).
    Table {
        description: TableDescription,
        default_values: HashMap<usize, Value>,
        column_ids: Vec<usize>,
    },
    /// A statement that reads the rows as `table_name`, with columns named `names`
    /// (DuckDB's `QueryAppender`).
    Query {
        query: roastedduck_common::ByteString,
        names: Vec<roastedduck_common::ByteString>,
        table_name: roastedduck_common::ByteString,
    },
}

/// Appends rows a value at a time, or a chunk at a time (DuckDB's `BaseAppender`).
#[derive(Debug)]
pub struct Appender {
    target: AppenderTarget,
    /// The types of all columns.
    types: Vec<LogicalType>,
    /// The types of the columns appended to, when not all (`active_types`).
    active_types: Vec<LogicalType>,
    /// The row being appended and the rows before it.
    chunk: DataChunk,
    /// The full chunks waiting for a flush (`collection`).
    collection: Vec<DataChunk>,
    /// The next column of the row being appended.
    column: usize,
}

/// The rows gathered before a flush (`BaseAppender::DEFAULT_FLUSH_COUNT`).
const DEFAULT_FLUSH_COUNT: usize = STANDARD_VECTOR_SIZE * 100;

/// The name the gathered rows have in a table appender's statement.
const APPENDED_DATA_TABLE: &str = "__duckdb_internal_appended_data";

fn invalid_input(message: impl Into<String>) -> Error {
    Error::invalid_input(message.into())
}

impl Appender {
    /// An appender to `table` in `schema` of `database` (an empty name is the default
    /// database) (`Appender::Appender`).
    pub fn new(
        connection: &mut Connection,
        database: impl AsRef<[u8]>,
        schema: impl AsRef<[u8]>,
        table: impl AsRef<[u8]>,
    ) -> Result<Self> {
        let (database, schema, table) = (database.as_ref(), schema.as_ref(), table.as_ref());
        let Some(description) = connection.table_info(database, schema, table)? else {
            return Err(Error::new_bytes(
                roastedduck_common::ErrorKind::Catalog,
                roastedduck_common::byte_format!("Table \"{database}.{schema}.{table}\" could not be found"),
            ));
        };
        if description.readonly {
            return Err(invalid_input("Cannot append to a readonly database."));
        }
        let physical: Vec<&ColumnDescription> = description
            .columns
            .iter()
            .filter(|column| !column.generated)
            .collect();
        let types = physical
            .iter()
            .map(|column| column.logical_type.clone())
            .collect();
        let default_values = connection.constant_defaults(&physical)?;
        Ok(Self::with_target(
            AppenderTarget::Table {
                description,
                default_values,
                column_ids: Vec::new(),
            },
            types,
        ))
    }

    /// An appender whose rows `query` reads as `table_name` (`appended_data` when empty),
    /// with columns of `types` named `names` (`col1`, ... when empty) (`QueryAppender`).
    pub fn for_query(query: String, types: Vec<LogicalType>, names: Vec<String>, table_name: String) -> Self {
        Self::for_query_bytes(
            query.into(),
            types,
            names.into_iter().map(Into::into).collect(),
            table_name.into(),
        )
    }

    /// Native C-string query/name input for `QueryAppender` clients.
    pub fn for_query_bytes(
        query: roastedduck_common::ByteString,
        types: Vec<LogicalType>,
        names: Vec<roastedduck_common::ByteString>,
        table_name: roastedduck_common::ByteString,
    ) -> Self {
        Self::with_target(
            AppenderTarget::Query {
                query,
                names,
                table_name,
            },
            types,
        )
    }

    fn with_target(target: AppenderTarget, types: Vec<LogicalType>) -> Self {
        let chunk = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
        Self {
            target,
            types,
            active_types: Vec::new(),
            chunk,
            collection: Vec::new(),
            column: 0,
        }
    }

    /// The types of the columns appended to (`GetActiveTypes`).
    pub fn active_types(&self) -> &[LogicalType] {
        if self.active_types.is_empty() {
            &self.types
        } else {
            &self.active_types
        }
    }

    /// Whether this appender writes through a client's statement (a `QueryAppender`).
    pub fn is_query_appender(&self) -> bool {
        matches!(self.target, AppenderTarget::Query { .. })
    }

    /// Whether a row is being appended or rows wait for a flush.
    pub fn has_pending_rows(&self) -> bool {
        self.column != 0 || !self.chunk.is_empty() || !self.collection.is_empty()
    }

    /// Starts over with empty chunks of the active types (`InitializeChunk` and a new
    /// collection).
    fn reinitialize(&mut self) {
        self.chunk = DataChunk::with_types(self.active_types(), STANDARD_VECTOR_SIZE);
        self.collection.clear();
    }

    /// Appends `value` to the next column of the row (`Append<Value>`): it is cast to the
    /// column's type.
    pub fn append_value(&mut self, value: &Value) -> Result<()> {
        let Some(ty) = self.active_types().get(self.column).cloned() else {
            return Err(invalid_input("Too many appends for chunk!"));
        };
        let value = if value.logical_type() == &ty {
            value.clone()
        } else {
            value.default_cast_as(&ty)?
        };
        let row = self.chunk.len();
        self.chunk.column_mut(self.column).set_value(row, &value)?;
        self.column += 1;
        Ok(())
    }

    /// Appends `value`, a value of one of the C types of the typed appends, to the next
    /// column of the row (`Append<T>`, `AppendValueInternal<T>`): columns of the common
    /// types take it through DuckDB's direct casts (see [`cast`]), others as
    /// [`Appender::append_value`] does.
    pub fn append_typed(&mut self, value: &Value) -> Result<()> {
        let Some(ty) = self.active_types().get(self.column) else {
            return Err(invalid_input("Too many appends for chunk!"));
        };
        let Some(value) = cast::cast_operation(value, ty)? else {
            return self.append_value(value);
        };
        let row = self.chunk.len();
        self.chunk.column_mut(self.column).set_value(row, &value)?;
        self.column += 1;
        Ok(())
    }

    /// Appends the string `bytes` to the next column of the row (`Append(const char *,
    /// uint32_t)`, `AppendValueInternal<string_t>`). A VARCHAR column takes the bytes as
    /// they are, as DuckDB's `StringCast` does; invalid UTF-8 is caught by the segment
    /// statistics when the rows are flushed. The columns without a direct cast take the
    /// VARCHAR value, whose construction validates the bytes.
    pub fn append_string(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(ty) = self.active_types().get(self.column) else {
            return Err(invalid_input("Too many appends for chunk!"));
        };
        let input = Value::bytes(LogicalType::VARCHAR, bytes);
        let Some(value) = cast::cast_operation(&input, ty)? else {
            return self.append_value(&Value::try_varchar(bytes)?);
        };
        let row = self.chunk.len();
        self.chunk.column_mut(self.column).set_value(row, &value)?;
        self.column += 1;
        Ok(())
    }

    /// Appends NULL to the next column of the row (`Append<nullptr_t>`).
    pub fn append_null(&mut self) -> Result<()> {
        let Some(ty) = self.active_types().get(self.column).cloned() else {
            return Err(invalid_input("Too many appends for chunk!"));
        };
        let row = self.chunk.len();
        self.chunk
            .column_mut(self.column)
            .set_value(row, &Value::null(ty))?;
        self.column += 1;
        Ok(())
    }

    /// The default value of column `column` of the appended columns (`GetDefaultValue`).
    fn default_value(&self, column: usize) -> Result<Value> {
        let AppenderTarget::Table {
            description,
            default_values,
            column_ids,
        } = &self.target
        else {
            return Err(Error::not_implemented(
                "AppendDefault is only supported when directly appending to a table",
            ));
        };
        let index = if column_ids.is_empty() {
            column
        } else {
            *column_ids
                .get(column)
                .ok_or_else(|| invalid_input("Column index out of bounds"))?
        };
        default_values.get(&index).cloned().ok_or_else(|| {
            let name = description
                .columns
                .get(index)
                .map_or(b"".as_slice(), |column| column.name.as_bytes());
            Error::not_implemented(roastedduck_common::byte_format!(
                "AppendDefault is not supported for column \"{name}\": not a foldable default expressions."
            ))
        })
    }

    /// Appends the column's default to the next column of the row (`AppendDefault`).
    pub fn append_default(&mut self) -> Result<()> {
        let value = self.default_value(self.column)?;
        if self.column >= self.active_types().len() {
            return Err(invalid_input("Too many appends for chunk!"));
        }
        self.append_value(&value)
    }

    /// Writes the default of appended column `column` into row `row` of `chunk`
    /// (`AppendDefault(DataChunk &, col, row)`).
    pub fn append_default_to_chunk(&self, chunk: &mut DataChunk, column: usize, row: usize) -> Result<()> {
        let value = self.default_value(column)?;
        if column >= chunk.column_count() {
            return Err(invalid_input("Too many appends for chunk!"));
        }
        let vector = chunk.column_mut(column);
        if row >= vector.capacity() {
            return Err(invalid_input("Too many rows for chunk!"));
        }
        let ty = vector.logical_type().clone();
        let value = if value.logical_type() == &ty {
            value
        } else {
            value.try_cast(&ty).map_err(|_| {
                invalid_input(format!(
                    "type mismatch in Append, expected {ty}, got {} for column {column}",
                    value.logical_type()
                ))
            })?
        };
        vector.set_value(row, &value)
    }

    /// Ends the row (`EndRow`); a full chunk joins the rows waiting for a flush, and so
    /// many rows waiting flush them.
    pub fn end_row(&mut self, connection: &mut Connection) -> Result<()> {
        if self.column != self.chunk.column_count() {
            return Err(invalid_input(
                "Call to EndRow before all columns have been appended to!",
            ));
        }
        self.column = 0;
        self.chunk.set_len(self.chunk.len() + 1);
        if self.chunk.len() >= STANDARD_VECTOR_SIZE {
            self.flush_chunk(connection)?;
        }
        Ok(())
    }

    /// Moves the rows of the chunk to the rows waiting for a flush (`FlushChunk`).
    fn flush_chunk(&mut self, connection: &mut Connection) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        self.collect_chunk();
        if self.collected_rows() >= DEFAULT_FLUSH_COUNT {
            self.flush(connection)?;
        }
        Ok(())
    }

    fn collected_rows(&self) -> usize {
        self.collection.iter().map(DataChunk::len).sum()
    }

    /// Appends the rows of `chunk`, cast to the appended columns' types
    /// (`AppendDataChunk`).
    pub fn append_data_chunk(&mut self, connection: &mut Connection, chunk: &DataChunk) -> Result<()> {
        let types = self.active_types().to_vec();
        if chunk.column_count() != types.len() {
            return Err(invalid_input(format!(
                "incorrect column count in AppendDataChunk, expected {}, got {}",
                types.len(),
                chunk.column_count()
            )));
        }
        let count = chunk.len();
        let mut columns = Vec::with_capacity(types.len());
        for (index, (column, ty)) in chunk.columns().iter().zip(&types).enumerate() {
            if column.logical_type() == ty {
                columns.push(column.flattened(count));
                continue;
            }
            let mut params = roastedduck_function::cast::CastParameters::try_cast();
            let cast = roastedduck_function::cast::cast_vector(
                &roastedduck_function::cast::CastFunctionSet::new(),
                column,
                ty,
                count,
                &mut params,
            );
            match cast {
                Ok(cast) if params.take_error_message().is_none() => columns.push(cast),
                _ => {
                    return Err(invalid_input(format!(
                        "type mismatch in AppendDataChunk, expected {ty}, got {} for column {index}",
                        column.logical_type()
                    )));
                }
            }
        }
        self.collection.push(DataChunk::new(columns, count));
        if self.collected_rows() >= DEFAULT_FLUSH_COUNT {
            self.flush(connection)?;
        }
        Ok(())
    }

    /// Writes the rows waiting (`Flush`): fails while a row is incomplete.
    pub fn flush(&mut self, connection: &mut Connection) -> Result<()> {
        if self.column != 0 {
            return Err(invalid_input(
                "Failed to Flush appender: incomplete append to row!",
            ));
        }
        if !self.chunk.is_empty() {
            self.collect_chunk();
        }
        if self.collected_rows() == 0 {
            return Ok(());
        }
        // The rows stay gathered until they are written, as a failed flush keeps them.
        let collection = ColumnDataCollection::new(self.active_types().to_vec(), self.collection.clone());
        let statement = self.flush_statement(collection)?;
        connection.append_statement(statement)?;
        self.collection.clear();
        self.column = 0;
        Ok(())
    }

    /// Moves the chunk's rows to the rows waiting for a flush, and starts an empty chunk.
    fn collect_chunk(&mut self) {
        let empty = DataChunk::with_types(self.active_types(), STANDARD_VECTOR_SIZE);
        let full = std::mem::replace(&mut self.chunk, empty);
        self.collection.push(full);
    }

    /// The statement that writes the gathered rows (`FlushInternal`).
    fn flush_statement(&self, collection: ColumnDataCollection) -> Result<SqlStatement> {
        match &self.target {
            AppenderTarget::Table {
                description,
                column_ids,
                ..
            } => {
                let names: Vec<roastedduck_common::ByteString> = column_ids
                    .iter()
                    .map(|&index| description.columns[index].name.clone())
                    .collect();
                let query = insert_query(description, APPENDED_DATA_TABLE, &names);
                parse_with_appended_data(&query, collection, APPENDED_DATA_TABLE.as_bytes(), names)
            }
            AppenderTarget::Query {
                query,
                names,
                table_name,
            } => {
                let alias = if table_name.is_empty() {
                    b"appended_data".as_slice()
                } else {
                    table_name.as_bytes()
                };
                parse_with_appended_data(query.as_bytes(), collection, alias, names.clone())
            }
        }
    }

    /// Flushes the rows waiting unless a row is incomplete (`Close`).
    pub fn close(&mut self, connection: &mut Connection) -> Result<()> {
        if self.column == 0 || self.column == self.active_types().len() {
            self.flush(connection)?;
        }
        Ok(())
    }

    /// Drops the rows appended and not flushed yet (`Clear`).
    pub fn clear(&mut self) {
        self.chunk.reset();
        self.collection.clear();
        self.column = 0;
    }

    /// Appends to column `name` too (`AddColumn`): the appended columns are those added,
    /// in order of addition, and the others get their defaults.
    pub fn add_column(&mut self, connection: &mut Connection, name: impl AsRef<[u8]>) -> Result<()> {
        let name = name.as_ref();
        if !matches!(self.target, AppenderTarget::Table { .. }) {
            return Err(Error::not_implemented(
                "AddColumn is only supported when directly appending to a table",
            ));
        }
        self.flush(connection)?;
        let AppenderTarget::Table {
            description,
            column_ids,
            ..
        } = &mut self.target
        else {
            unreachable!("checked to be a table appender above")
        };
        let Some(index) = description
            .columns
            .iter()
            .position(|column| column.name.as_bytes() == name)
        else {
            return Err(invalid_input("the column must exist in the table"));
        };
        let column = &description.columns[index];
        if column.generated {
            return Err(invalid_input("cannot add a generated column to the appender"));
        }
        if column_ids.contains(&index) {
            return Err(invalid_input("cannot add the same column twice"));
        }
        self.active_types.push(column.logical_type.clone());
        column_ids.push(index);
        self.reinitialize();
        Ok(())
    }

    /// Appends to every column again (`ClearColumns`).
    pub fn clear_columns(&mut self, connection: &mut Connection) -> Result<()> {
        if !matches!(self.target, AppenderTarget::Table { .. }) {
            return Err(Error::not_implemented(
                "ClearColumns is only supported when directly appending to a table",
            ));
        }
        self.flush(connection)?;
        if let AppenderTarget::Table { column_ids, .. } = &mut self.target {
            column_ids.clear();
        }
        self.active_types.clear();
        self.reinitialize();
        Ok(())
    }
}

/// `INSERT INTO [database.][schema.]table[(names)] FROM table_name`
/// (`Appender::ConstructQuery`).
fn insert_query(
    description: &TableDescription,
    table_name: &str,
    names: &[roastedduck_common::ByteString],
) -> roastedduck_common::ByteString {
    let quoted = roastedduck_ast::keyword::write_optionally_quoted_bytes;
    let mut query = roastedduck_common::ByteString::from("INSERT INTO ");
    if !description.database.is_empty() {
        query.push_bytes(quoted(&description.database));
        query.push(b'.');
    }
    if !description.schema.is_empty() {
        query.push_bytes(quoted(&description.schema));
        query.push(b'.');
    }
    query.push_bytes(quoted(&description.table));
    if !names.is_empty() {
        let names: Vec<roastedduck_common::ByteString> = names.iter().map(|name| quoted(name)).collect();
        query.push(b'(');
        query.push_bytes(roastedduck_common::ByteString::join(&names, b", "));
        query.push(b')');
    }
    query.push_bytes(b" FROM ");
    query.push_bytes(table_name.as_bytes());
    query
}

/// Parses the single statement `query` and gives it `alias` as a common table
/// expression over the gathered rows, named `expected_names` (`BaseAppender::ParseStatement`
/// with `GetColumnDataTableRef`).
fn parse_with_appended_data(
    query: &[u8],
    collection: ColumnDataCollection,
    alias: &[u8],
    expected_names: Vec<roastedduck_common::ByteString>,
) -> Result<SqlStatement> {
    let mut statements = roastedduck_parser::parse_bytes(query)?;
    if statements.len() != 1 {
        return Err(invalid_input("Expected exactly one query for appending data."));
    }
    let mut statement = statements.remove(0);
    let mut table_ref = build::table_ref(TableRefKind::ColumnData(ColumnDataRef {
        expected_names,
        collection: Some(Box::new(collection)),
    }));
    table_ref.alias = alias.into();
    let mut select = build::select_node();
    select.select_list.push(build::star());
    *select.from_table = table_ref;
    let cte = CommonTableExpressionInfo {
        aliases: Vec::new(),
        query: Box::new(build::select_statement(build::query_node(QueryNodeKind::Select(
            Box::new(select),
        )))),
        materialized: CteMaterialize::CteMaterializeNever,
        key_targets: Vec::new(),
    };
    let cte_map = match &mut statement.kind {
        SqlStatementKind::Insert(insert) => &mut insert.cte_map,
        SqlStatementKind::Delete(delete) => &mut delete.cte_map,
        SqlStatementKind::Update(update) => &mut update.cte_map,
        SqlStatementKind::MergeInto(merge) => &mut merge.cte_map,
        _ => {
            return Err(invalid_input(
                "Unsupported statement type for appender: expected INSERT, DELETE, UPDATE or MERGE INTO",
            ));
        }
    };
    cte_map.map.insert(alias.to_owned(), cte);
    Ok(statement)
}
