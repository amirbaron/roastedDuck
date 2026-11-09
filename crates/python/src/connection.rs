//! Python connection/cursor ownership and lazy result fetching over the native Rust runtime.
use crate::datatype::PyType;
use crate::pending::PythonExecution;
use crate::{conversion, error};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use roastedduck::{
    AccessMode, ClientHandle, Config, Connection, DataChunk, Database, LogicalType, QueryResult,
};
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result};
use roastedduck_types::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
#[derive(Debug)]
struct CachedDatabase {
    config: Config,
    python_options: Vec<(String, Value)>,
    database: Weak<Database>,
}
static DATABASES: OnceLock<Mutex<HashMap<ByteString, CachedDatabase>>> = OnceLock::new();
fn platform_cache_key(key: &[u8]) -> ByteString {
    let key = ByteString::from(key);
    if cfg!(any(target_vendor = "apple", windows)) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}
fn cache_key(path: &Path) -> Result<ByteString> {
    if path.as_os_str().as_encoded_bytes().starts_with(b":memory:") {
        // Named-memory identities are process-local names, not filesystem paths:
        // their sharing must survive chdir without opening or resolving a file.
        // PathBuf equality also folds separators and dot components. Names must
        // retain those bytes, just as C++'s string-keyed cache does.
        return Ok(platform_cache_key(path.as_os_str().as_encoded_bytes()));
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::new(roastedduck_common::ErrorKind::Io, e.to_string()))?
            .join(path)
    };
    let mut key = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                key.pop();
            }
            other => key.push(other),
        }
    }
    let key = std::fs::canonicalize(&key).unwrap_or(key);
    Ok(platform_cache_key(key.as_os_str().as_encoded_bytes()))
}
fn open_database(path: &Path, config: &Config, extras: &[(String, Value)]) -> Result<Arc<Database>> {
    let create = || -> Result<Arc<Database>> {
        // The pool retains the name as its key; the engine opens a true memory
        // instance, matching C++ DBInstanceCache rather than a colon-named file.
        let original = path.as_os_str().as_encoded_bytes();
        let instance_path = if original.starts_with(b":memory:") {
            b":memory:".as_slice()
        } else {
            original
        };
        let database = Arc::new(Database::open(instance_path, config)?);
        for (name, value) in extras {
            database.add_extension_option(roastedduck_function::ExtensionOption{name:name.clone(),description:match name.as_str(){"pandas_analyze_sample"=>"The maximum number of rows to sample when analyzing a pandas object column.","python_enable_replacements"=>"Whether variables visible to the current stack should be used for replacement scans.",_=>"If set, restores the old behavior of scanning all preceding frames to locate the referenced variable."}.into(),input_type:value.logical_type().clone(),default_value:value.clone(),set_function:None,global_by_default:true});
        }
        database.add_replacement_scan(crate::input::replacement_scan());
        Ok(database)
    };
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return create();
    }
    let key = cache_key(path)?;
    let mut pool = lock(DATABASES.get_or_init(Mutex::default));
    if let Some(cached) = pool.get(&key)
        && let Some(database) = cached.database.upgrade()
    {
        if cached.config != *config || cached.python_options != extras {
            return Err(Error::new(
                roastedduck_common::ErrorKind::Connection,
                "Can't open a connection to same database file with a different configuration than existing connections",
            ));
        }
        return Ok(database);
    }
    let database = create()?;
    pool.insert(
        key,
        CachedDatabase {
            config: config.clone(),
            python_options: extras.to_vec(),
            database: Arc::downgrade(&database),
        },
    );
    Ok(database)
}
static CURSOR_IDS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub(super) struct RowCheckpoint {
    cursor_id: u64,
    epoch: u64,
}
pub(super) type FetchedRow = (Option<Vec<Value>>, String, Option<RowCheckpoint>);

#[derive(Debug)]
pub(super) struct ResultCursor {
    id: u64,
    row_epoch: u64,
    result: QueryResult,
    chunk: Option<DataChunk>,
    next_chunk: usize,
    bulk_chunk: usize,
    bulk_started: bool,
    row: usize,
    exhausted: bool,
    bulk_consumed: bool,
    properties: roastedduck_arrow::ClientProperties,
}
impl ResultCursor {
    pub(super) fn is_open(&self, connection: &Connection) -> bool {
        match &self.result {
            QueryResult::Stream(stream) => connection.stream_is_open(stream),
            QueryResult::Materialized(_) => true,
        }
    }
    pub(super) fn new(result: QueryResult, properties: roastedduck_arrow::ClientProperties) -> Self {
        Self {
            id: CURSOR_IDS.fetch_add(1, Ordering::Relaxed),
            row_epoch: 0,
            result,
            chunk: None,
            next_chunk: 0,
            bulk_chunk: 0,
            bulk_started: false,
            row: 0,
            exhausted: false,
            bulk_consumed: false,
            properties,
        }
    }
    pub(super) fn names(&self) -> &[ByteString] {
        match &self.result {
            QueryResult::Materialized(r) => r.names(),
            QueryResult::Stream(r) => r.names(),
        }
    }
    pub(super) fn timezone(&self) -> &str {
        &self.properties.time_zone
    }
    pub(super) fn row_checkpoint(&self) -> RowCheckpoint {
        RowCheckpoint {
            cursor_id: self.id,
            epoch: self.row_epoch,
        }
    }
    /// Python conversion precedes advancing DuckDB's row offset. Undo our fetch
    /// only while the same cursor has not been used or replaced by another call.
    pub(super) fn restore_row(&mut self, checkpoint: RowCheckpoint) {
        if self.id == checkpoint.cursor_id && self.row_epoch == checkpoint.epoch && self.row > 0 {
            self.row -= 1;
            self.row_epoch = self.row_epoch.wrapping_add(1);
        }
    }
    pub(super) fn begin_bulk(&mut self) -> crate::host::Metadata {
        self.row_epoch = self.row_epoch.wrapping_add(1);
        let metadata = crate::host::Metadata {
            names: self.names().to_vec(),
            types: self.types().to_vec(),
            timezone: self.properties.time_zone.clone(),
            consumed: self.bulk_consumed,
            materialized: matches!(self.result, QueryResult::Materialized(_)),
            materialized_empty: matches!(&self.result, QueryResult::Materialized(result) if result.row_count() == 0),
            arrow: self.properties.clone(),
        };
        self.bulk_chunk = 0;
        self.bulk_started = false;
        // Materialized NumPy conversion iterates the collection independently.
        // Its failure must preserve any existing row-fetch position.
        // Do not discard a row chunk until bulk fetching actually starts: an
        // unsupported dtype can fail during initialization without consuming it.
        metadata
    }
    pub(super) fn bulk_next(&mut self, connection: &mut Connection) -> Result<Option<DataChunk>> {
        self.row_epoch = self.row_epoch.wrapping_add(1);
        if self.bulk_consumed {
            return Ok(None);
        }
        if !self.bulk_started {
            self.bulk_started = true;
            if matches!(self.result, QueryResult::Stream(_)) {
                self.chunk = None;
                self.row = 0;
            }
        }
        let chunk = match &self.result {
            QueryResult::Stream(stream) if connection.stream_is_open(stream) => connection.fetch(stream)?,
            QueryResult::Materialized(result) if self.bulk_chunk < result.chunk_count() => {
                let chunk = result.chunk(self.bulk_chunk)?.into_owned();
                self.bulk_chunk += 1;
                Some(chunk)
            }
            QueryResult::Stream(_) | QueryResult::Materialized(_) => None,
        };
        if chunk.is_none() {
            self.bulk_consumed = true;
            self.exhausted = true;
            if let QueryResult::Materialized(result) = &mut self.result {
                self.chunk = None;
                self.row = 0;
                *result = roastedduck::MaterializedResult::new(
                    result.names().to_vec(),
                    result.types().to_vec(),
                    Vec::new(),
                );
                self.next_chunk = 0;
            }
        }
        Ok(chunk)
    }
    pub(super) fn types(&self) -> &[LogicalType] {
        match &self.result {
            QueryResult::Materialized(r) => r.types(),
            QueryResult::Stream(r) => r.types(),
        }
    }
    pub(super) fn close(self, connection: &mut Connection) {
        if let QueryResult::Stream(stream) = self.result {
            connection.close_query(stream.id());
        }
    }
    pub(super) fn next(&mut self, connection: &mut Connection) -> Result<Option<Vec<Value>>> {
        self.row_epoch = self.row_epoch.wrapping_add(1);
        if self.exhausted {
            return Ok(None);
        }
        loop {
            if let Some(chunk) = &self.chunk
                && self.row < chunk.len()
            {
                let row = (0..chunk.column_count())
                    .map(|i| chunk.column(i).get_value(self.row))
                    .collect();
                self.row += 1;
                return Ok(Some(row));
            }
            self.chunk = match &self.result {
                QueryResult::Stream(stream) if connection.stream_is_open(stream) => {
                    connection.fetch(stream)?
                }
                QueryResult::Stream(_) => None,
                QueryResult::Materialized(result) => {
                    if self.next_chunk < result.chunk_count() {
                        let chunk = result.chunk(self.next_chunk)?.into_owned();
                        self.next_chunk += 1;
                        Some(chunk)
                    } else {
                        None
                    }
                }
            };
            self.row = 0;
            if self.chunk.is_none() {
                self.exhausted = true;
                return Ok(None);
            }
        }
    }
}
type ResultMetadata = (Vec<ByteString>, Vec<LogicalType>);

#[derive(Debug)]
pub(super) struct State {
    closed: Arc<AtomicBool>,
    database: Option<Arc<Database>>,
    connection: Option<Arc<Mutex<Connection>>>,
    result: Option<ResultCursor>,
    transferred: Option<ResultMetadata>,
    children: Vec<Weak<Mutex<State>>>,
    registered: HashSet<ByteString>,
    functions: HashSet<ByteString>,
}
impl State {
    pub(super) fn reader_handle(&self) -> Result<Arc<Mutex<Connection>>> {
        self.connection.clone().ok_or_else(|| {
            Error::new(
                roastedduck_common::ErrorKind::Connection,
                "Connection already closed!",
            )
        })
    }
    fn host_metadata(&mut self) -> Result<crate::host::Metadata> {
        let missing = if self.transferred.is_some() {
            "result closed"
        } else {
            "No open result set"
        };
        let result = self
            .result
            .as_mut()
            .ok_or_else(|| Error::invalid_input(missing))?;
        Ok(result.begin_bulk())
    }
    fn host_chunk(&mut self) -> Result<Option<DataChunk>> {
        let missing = if self.transferred.is_some() {
            "result closed"
        } else {
            "No open result set"
        };
        let result = self
            .result
            .as_mut()
            .ok_or_else(|| Error::invalid_input(missing))?;
        let connection = self.connection.as_mut().ok_or_else(|| {
            Error::new(
                roastedduck_common::ErrorKind::Connection,
                "Connection already closed!",
            )
        })?;
        result.bulk_next(&mut lock(connection))
    }
    pub(super) fn relation_connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .as_ref()
            .map(|connection| lock(connection))
            .ok_or_else(|| {
                Error::new(
                    roastedduck_common::ErrorKind::Connection,
                    "Connection has already been closed",
                )
            })
    }
    pub(super) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .as_ref()
            .map(|connection| lock(connection))
            .ok_or_else(|| {
                Error::new(
                    roastedduck_common::ErrorKind::Connection,
                    "Connection already closed!",
                )
            })
    }
    pub(super) fn clear_result(&mut self) {
        self.transferred = None;
        if let Some(ResultCursor {
            result: QueryResult::Stream(stream),
            ..
        }) = self.result.take()
            && let Some(connection) = &mut self.connection
        {
            lock(connection).close_query(stream.id());
        }
    }
    pub(super) fn execute(
        &mut self,
        sql: &[u8],
        parameters: Option<CaseInsensitiveMap<Value, ByteString>>,
    ) -> Result<()> {
        self.execute_input(crate::statement::Query::Text(sql), parameters)
            .map(|_| ())
    }
    fn execute_input(
        &mut self,
        input: crate::statement::Query<'_>,
        parameters: Option<CaseInsensitiveMap<Value, ByteString>>,
    ) -> Result<bool> {
        self.clear_result();
        let mut connection = self.connection()?;
        let mut statements = input.statements(&mut connection)?;
        let Some(last) = statements.pop() else {
            return Ok(false);
        };
        for statement in statements {
            if !statement.named_param_map.is_empty() {
                return Err(Error::not_implemented(
                    "Prepared parameters are only supported for the last statement",
                ));
            }
            let query = statement.query.clone();
            connection.execute_python_statement(query, statement)?;
        }
        let result = if parameters.is_some() || !last.named_param_map.is_empty() {
            let parameters = parameters.unwrap_or_default();
            let prepared = connection.prepare_statement(last)?;
            let pending = connection.pending_prepared(&prepared, &parameters, true)?;
            crate::pending::complete(&mut connection, &pending)?
        } else {
            let query = last.query.clone();
            let pending = connection.pending_query(query, last, true)?;
            crate::pending::complete(&mut connection, &pending)?
        };
        let properties = connection.client_properties();
        drop(connection);
        self.result = Some(ResultCursor::new(result, properties));
        Ok(true)
    }
    fn next_with_timezone(&mut self) -> Result<FetchedRow> {
        let row = self.next()?;
        let zone = self
            .result
            .as_ref()
            .ok_or_else(|| Error::invalid_input("No open result set"))?
            .timezone()
            .to_owned();
        let checkpoint = row
            .as_ref()
            .and_then(|_| self.result.as_ref().map(ResultCursor::row_checkpoint));
        Ok((row, zone, checkpoint))
    }
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        let missing = if self.transferred.is_some() {
            "result closed"
        } else {
            "No open result set"
        };
        let result = self
            .result
            .as_mut()
            .ok_or_else(|| Error::invalid_input(missing))?;
        let connection = self.connection.as_mut().ok_or_else(|| {
            Error::new(
                roastedduck_common::ErrorKind::Connection,
                "Connection already closed!",
            )
        })?;
        result.next(&mut lock(connection))
    }
}
#[pyclass(module = "_duckdb", name = "DuckDBPyConnection", frozen)]
#[derive(Debug)]
pub struct PyConnection {
    pub(super) state: Arc<Mutex<State>>,
    pub(super) id: u64,
    client: Arc<ClientHandle>,
    closed: Arc<AtomicBool>,
}
impl PyConnection {
    fn named_relation<'py>(
        slf: Bound<'py, Self>,
        method: &str,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        let field = if method == "view" {
            "view_name"
        } else {
            "table_name"
        };
        let signature =
            format!("(self: _duckdb.DuckDBPyConnection, {field}: str) -> _duckdb.DuckDBPyRelation");
        let values =
            crate::arguments::fields(method, &signature, slf.as_any(), args, kwargs, &[field], 1, 1)?;
        let value = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated name"))?;
        let Some(name) = crate::arguments::string_bytes(value)? else {
            return Err(error::arguments(
                slf.py(),
                method,
                &signature,
                Some(slf.as_any()),
                args,
                kwargs,
            )?);
        };
        slf.get().check_client_open(slf.py())?;
        crate::relation::PyRelation::from_table(slf, &name, method == "view")
    }

    fn query_relation<'py>(
        slf: Bound<'py, Self>,
        name: &str,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Option<crate::relation::PyRelation>> {
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyConnection, query: object, *, alias: str = '', params: object = None) -> _duckdb.DuckDBPyRelation";
        let values = crate::arguments::fields(
            name,
            signature,
            slf.as_any(),
            args,
            kwargs,
            &["query", "alias", "params"],
            1,
            1,
        )?;
        let alias = match values[1].as_ref() {
            Some(value) => match crate::arguments::string_bytes(value)? {
                Some(bytes) => bytes,
                None => {
                    return Err(error::arguments(
                        py,
                        name,
                        signature,
                        Some(slf.as_any()),
                        args,
                        kwargs,
                    )?);
                }
            },
            None => Vec::new(),
        };
        slf.get().check_client_open(py)?;
        let query = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated query"))?;
        let input = crate::statement::input(query)?;
        let params = values[2].as_ref().filter(|value| !value.is_none());
        crate::relation::PyRelation::from_input(slf, input, &alias, params)
    }

    pub(super) fn named_type(&self, py: Python<'_>, name: &[u8]) -> PyResult<PyType> {
        self.check_client_open(py)?;
        let kind = py
            .detach(|| {
                if let Some(kind) = Connection::type_from_current_query(self.id, name) {
                    return kind;
                }
                lock(&self.state).connection()?.type_from_name_bytes(name)
            })
            .map_err(|error| crate::error::native(py, &error))?;
        Ok(PyType { native: kind })
    }
    fn command<'py>(slf: Bound<'py, Self>, sql: &str) -> PyResult<Bound<'py, Self>> {
        let py = slf.py();
        let state = &slf.get().state;
        py.detach(|| lock(state).execute(sql.as_bytes(), None))
            .map_err(|e| error::native(py, &e))?;
        Ok(slf)
    }

    fn check_client_open(&self, py: Python<'_>) -> PyResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(error::native(
                py,
                &Error::new(
                    roastedduck_common::ErrorKind::Connection,
                    "Connection already closed!",
                ),
            ));
        }
        Ok(())
    }

    fn arrow_reader(&self, py: Python<'_>, batch_size: usize, capsule: bool) -> PyResult<Py<PyAny>> {
        let (connection, result) = py
            .detach(|| {
                let mut state = lock(&self.state);
                let missing = if state.transferred.is_some() {
                    "There is no query result"
                } else {
                    "No open result set"
                };
                let result = state.result.take().ok_or_else(|| Error::invalid_input(missing))?;
                state.transferred = Some((result.names().to_vec(), result.types().to_vec()));
                Ok::<_, Error>((state.reader_handle()?, result))
            })
            .map_err(|e| error::native(py, &e))?;
        crate::reader::export(py, connection, result, batch_size, capsule)
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
            .detach(|| lock(&self.state).host_metadata())
            .map_err(|e| error::native(py, &e))?;
        if deduplicate {
            metadata.names = crate::host::names(&metadata.names);
        }
        crate::host::arrow(py, &metadata, rows_per_batch, || {
            py.detach(|| lock(&self.state).host_chunk())
                .map_err(|e| error::native(py, &e))
        })
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
            .detach(|| lock(&self.state).host_metadata())
            .map_err(|e| error::native(py, &e))?;
        crate::host::fetch(py, &metadata, pandas, false, date_as_object, max_chunks, || {
            py.detach(|| lock(&self.state).host_chunk())
                .map_err(|e| error::native(py, &e))
        })
    }
    fn from_database(database: Arc<Database>) -> Result<Self> {
        let mut connection = database.connect()?;
        for functions in crate::input::functions()? {
            connection.replace_table_function_set(functions)?;
        }
        connection.create_table_function_set(crate::pandas::functions())?;
        connection.create_table_function_set(crate::map::functions())?;
        let client = connection.client_handle();
        let id = connection.connection_id();
        let closed = Arc::new(AtomicBool::new(false));
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                closed: Arc::clone(&closed),
                database: Some(database),
                connection: Some(Arc::new(Mutex::new(connection))),
                result: None,
                transferred: None,
                children: Vec::new(),
                registered: HashSet::new(),
                functions: HashSet::new(),
            })),
            client,
            closed,
            id,
        })
    }
    pub(super) fn parameters(
        parameters: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<CaseInsensitiveMap<Value, ByteString>>> {
        let Some(parameters) = parameters.filter(|p| !p.is_none()) else {
            return Ok(None);
        };
        let mut values = CaseInsensitiveMap::default();
        if let Ok(dict) = parameters.cast::<PyDict>() {
            for (key, value) in dict.iter() {
                values.insert(key.str()?.to_str()?.as_bytes(), conversion::from_python(&value)?);
            }
        } else if parameters.is_instance_of::<PyList>() || parameters.is_instance_of::<PyTuple>() {
            for (i, value) in parameters.try_iter()?.enumerate() {
                values.insert((i + 1).to_string().as_bytes(), conversion::from_python(&value?)?);
            }
        } else {
            return Err(error::InvalidInputException::new_err(
                "Invalid Input Error: Prepared parameters can only be passed as a list or a dictionary",
            ));
        }
        Ok(Some(values))
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
                    && let Some(result) = lock(&self.state).result.as_mut()
                {
                    result.restore_row(checkpoint);
                }
            });
        }
        converted
    }
    fn fetch_rows(&self, py: Python<'_>, limit: Option<usize>) -> PyResult<Py<PyAny>> {
        let rows = PyList::empty(py);
        let mut fetched = 0;
        loop {
            if limit.is_some_and(|limit| fetched >= limit) {
                break;
            }
            let (row, timezone, checkpoint) = py
                .detach(|| lock(&self.state).next_with_timezone())
                .map_err(|e| error::native(py, &e))?;
            let Some(row) = row else {
                break;
            };
            rows.append(self.row_to_python(py, &row, &timezone, checkpoint)?)?;
            fetched += 1;
        }
        Ok(rows.unbind().into_any())
    }
}
fn self_check_relation(
    owner: &Bound<'_, PyConnection>,
    object: &Bound<'_, PyAny>,
    name: impl AsRef<[u8]>,
) -> PyResult<()> {
    let py = owner.py();
    owner.get().check_client_open(py)?;
    if let Ok(relation) = object.extract::<PyRef<'_, crate::relation::PyRelation>>()
        && relation.connection_id(py) != owner.get().id
    {
        return Err(error::native(
            py,
            &Error::invalid_input(roastedduck_common::byte_format!(
                "Python Object \"{}\" of type \"DuckDBPyRelation\" not suitable for replacement scan.\nThe object was created by another Connection and can therefore not be used by this Connection.",
                name.as_ref()
            )),
        ));
    }
    Ok(())
}
#[pymethods]
impl PyConnection {
    #[pyo3(signature=(query,parameters=None))]
    fn execute<'py>(
        slf: Bound<'py, Self>,
        query: &Bound<'_, PyAny>,
        parameters: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Bound<'py, Self>>> {
        let py = slf.py();
        let state = &slf.get().state;
        let input = match crate::statement::input(query) {
            Ok(input) => input,
            Err(error) => {
                py.detach(|| lock(state).clear_result());
                return Err(error);
            }
        };
        let values = match Self::parameters(parameters) {
            Ok(values) => values,
            Err(error) => {
                py.detach(|| lock(state).clear_result());
                return Err(error);
            }
        };
        let executed = py
            .detach(|| lock(state).execute_input(input, values))
            .map_err(|e| error::native(py, &e))?;
        Ok(executed.then_some(slf))
    }
    #[pyo3(signature=(query,parameters=None))]
    fn executemany<'py>(
        slf: Bound<'py, Self>,
        query: &Bound<'_, PyAny>,
        parameters: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Bound<'py, Self>>> {
        let py = slf.py();
        let state = &slf.get().state;
        py.detach(|| lock(state).clear_result());
        let input = crate::statement::input(query)?;
        let prepared = py
            .detach(|| {
                let state = lock(state);
                let mut connection = state.connection()?;
                let mut statements = input.statements(&mut connection)?;
                let Some(last) = statements.pop() else {
                    return Ok(None);
                };
                for statement in statements {
                    if !statement.named_param_map.is_empty() {
                        return Err(Error::not_implemented(
                            "Prepared parameters are only supported for the last statement",
                        ));
                    }
                    let query = statement.query.clone();
                    connection.execute_python_statement(query, statement)?;
                }
                connection.prepare_statement(last).map(Some)
            })
            .map_err(|e| error::native(py, &e))?;
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let batches = if let Some(parameters) = parameters.filter(|p| !p.is_none()) {
            let abc = py.import("collections.abc")?;
            if parameters.is_instance_of::<pyo3::types::PyString>()
                || parameters.is_instance_of::<pyo3::types::PyBytes>()
                || parameters.is_instance(&abc.getattr("Mapping")?)?
                || !parameters.is_instance(&abc.getattr("Iterable")?)?
            {
                return Err(error::native(
                    py,
                    &Error::invalid_input("executemany requires a list of parameter sets to be provided"),
                ));
            }
            py.import("builtins")?
                .getattr("list")?
                .call1((parameters,))?
                .cast_into::<PyList>()?
        } else {
            PyList::empty(py)
        };
        if batches.is_empty() {
            return Err(error::native(
                py,
                &Error::invalid_input(
                    "executemany requires a non-empty list of parameter sets to be provided",
                ),
            ));
        }
        let mut last = None;
        for batch in batches.iter() {
            let values = Self::parameters(Some(&batch))?.unwrap_or_default();
            last = Some(
                py.detach(|| {
                    let state = lock(state);
                    let mut connection = state.connection()?;
                    let pending = connection.pending_prepared(&prepared, &values, false)?;
                    let result = crate::pending::complete(&mut connection, &pending)?;
                    Ok::<_, Error>(ResultCursor::new(result, connection.client_properties()))
                })
                .map_err(|e| error::native(py, &e))?,
            );
        }
        py.detach(|| lock(state).result = last);
        Ok(Some(slf))
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn sql<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Option<crate::relation::PyRelation>> {
        Self::query_relation(slf, "sql", args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn query<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Option<crate::relation::PyRelation>> {
        Self::query_relation(slf, "query", args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn from_query<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Option<crate::relation::PyRelation>> {
        Self::query_relation(slf, "from_query", args, kwargs)
    }
    fn register_filesystem(slf: &Bound<'_, Self>, filesystem: &Bound<'_, PyAny>) -> PyResult<()> {
        crate::filesystem::register(slf, filesystem)
    }
    fn unregister_filesystem(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        crate::filesystem::unregister(self, py, name)
    }
    fn list_filesystems(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        crate::filesystem::names(self, py)
    }
    fn filesystem_is_registered(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        Ok(crate::filesystem::names(self, py)?.iter().any(|n| n == name))
    }
    fn extract_statements(&self, py: Python<'_>, query: &str) -> PyResult<Py<PyAny>> {
        let statements = py
            .detach(|| {
                lock(&self.state)
                    .connection()?
                    .parse_statements_bytes(query.as_bytes())
            })
            .map_err(|e| error::native(py, &e))?;
        crate::statement::list(py, statements)
    }
    #[pyo3(signature=(query,*,qualified=false))]
    fn get_table_names(&self, py: Python<'_>, query: &str, qualified: bool) -> PyResult<Py<PyAny>> {
        let names = py
            .detach(|| lock(&self.state).connection()?.table_names(query, qualified))
            .map_err(|e| error::native(py, &e))?;
        let set = pyo3::types::PySet::empty(py)?;
        for name in names {
            set.add(conversion::text(py, &name)?)?;
        }
        Ok(set.unbind().into_any())
    }
    #[pyo3(signature=(extension,*,force_install=None,repository=None,repository_url=None,version=None))]
    fn install_extension(
        &self,
        py: Python<'_>,
        extension: &str,
        force_install: Option<&Bound<'_, PyAny>>,
        repository: Option<&Bound<'_, PyAny>>,
        repository_url: Option<&Bound<'_, PyAny>>,
        version: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        self.check_client_open(py)?;
        let force_install = if let Some(flag) = force_install.filter(|flag| !flag.is_none()) {
            // SAFETY: numeric conversion inspects a live object under the GIL.
            if unsafe { pyo3::ffi::PyNumber_Check(flag.as_ptr()) } == 0 {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "force_install must be a boolean",
                ));
            }
            flag.is_truthy()?
        } else {
            false
        };

        let repository = repository.filter(|v| !v.is_none());
        let repository_url = repository_url.filter(|v| !v.is_none());
        if repository.is_some() && repository_url.is_some() {
            return Err(error::native(
                py,
                &Error::invalid_input(
                    "Both 'repository' and 'repository_url' are set which is not allowed, please pick one or the other",
                ),
            ));
        }
        let repo_is_alias = repository.is_some();
        let repository = repository
            .or(repository_url)
            .map(|r| r.str()?.extract::<String>())
            .transpose()?;
        if repository.as_ref().is_some_and(String::is_empty) {
            return Err(error::native(
                py,
                &Error::invalid_input("The provided 'repository' or 'repository_url' can not be empty!"),
            ));
        }
        let version = version
            .filter(|v| !v.is_none())
            .map(|v| v.str()?.extract::<String>())
            .transpose()?;
        if version.as_ref().is_some_and(String::is_empty) {
            return Err(error::native(
                py,
                &Error::invalid_input("The provided 'version' can not be empty!"),
            ));
        }
        let statement = roastedduck_ast::SqlStatement::new(roastedduck_ast::SqlStatementKind::Load(
            roastedduck_ast::LoadInfo {
                filename: extension.into(),
                load_type: if force_install {
                    roastedduck_ast::LoadType::ForceInstall
                } else {
                    roastedduck_ast::LoadType::Install
                },
                repository: repository.unwrap_or_default().into(),
                version: version.unwrap_or_default().into(),
                repo_is_alias,
            },
        ));
        py.detach(|| {
            lock(&self.state)
                .connection()?
                .execute_python_statement([], statement)
                .map(|_| ())
        })
        .map_err(|e| error::native(py, &e))
    }
    fn load_extension(&self, py: Python<'_>, extension: &str) -> PyResult<()> {
        py.detach(|| lock(&self.state).connection()?.load_extension(extension))
            .map_err(|e| error::native(py, &e))
    }
    #[pyo3(signature=(*args))]
    fn values(slf: Bound<'_, Self>, args: &Bound<'_, PyTuple>) -> PyResult<crate::relation::PyRelation> {
        slf.get().check_client_open(slf.py())?;
        crate::relation::PyRelation::from_values(slf, args)
    }
    #[pyo3(signature=(*args,**kwargs))]
    fn read_json(
        slf: Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        crate::json::read(slf, args, kwargs)
    }
    #[pyo3(signature=(path_or_buffer,**kwargs))]
    fn read_csv(
        slf: Bound<'_, Self>,
        path_or_buffer: &Bound<'_, PyAny>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        slf.get().check_client_open(slf.py())?;
        crate::files::read(slf, path_or_buffer, kwargs)
    }
    #[pyo3(signature=(path_or_buffer,**kwargs))]
    fn from_csv_auto(
        slf: Bound<'_, Self>,
        path_or_buffer: &Bound<'_, PyAny>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        slf.get().check_client_open(slf.py())?;
        crate::files::read(slf, path_or_buffer, kwargs)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, path_or_buffer, binary_as_string=False, *, file_row_number=False, filename=False, hive_partitioning=False, union_by_name=False, compression=None)")]
    fn read_parquet(
        slf: Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        slf.get().check_client_open(slf.py())?;
        crate::files::parquet_call(slf, args, kwargs, "read_parquet")
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, path_or_buffer, binary_as_string=False, *, file_row_number=False, filename=False, hive_partitioning=False, union_by_name=False, compression=None)")]
    fn from_parquet(
        slf: Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        slf.get().check_client_open(slf.py())?;
        crate::files::parquet_call(slf, args, kwargs, "from_parquet")
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn table<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        Self::named_relation(slf, "table", args, kwargs)
    }
    #[pyo3(signature=(*arguments, **kwargs))]
    fn table_function<'py>(
        slf: Bound<'py, Self>,
        arguments: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyConnection, name: str, parameters: object = None) -> _duckdb.DuckDBPyRelation";
        let values = crate::arguments::fields(
            "table_function",
            signature,
            slf.as_any(),
            arguments,
            kwargs,
            &["name", "parameters"],
            1,
            2,
        )?;
        let value = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated function name"))?;
        let Some(name) = crate::arguments::string_bytes(value)? else {
            return Err(error::arguments(
                py,
                "table_function",
                signature,
                Some(slf.as_any()),
                arguments,
                kwargs,
            )?);
        };
        slf.get().check_client_open(py)?;
        let parameters = values[1].as_ref().filter(|value| !value.is_none());
        let mut args = Vec::new();
        if let Some(parameters) = parameters {
            if !crate::files::list_like(parameters)? {
                return Err(error::native(
                    py,
                    &Error::invalid_input("'params' has to be a list of parameters"),
                ));
            }
            let _length = parameters.len()?;
            for value in parameters.try_iter()? {
                args.push(roastedduck_parser::build::constant(conversion::from_python(
                    &value?,
                )?));
            }
        }
        let function = roastedduck_parser::build::function(name, args);
        let mut table = roastedduck_parser::build::table_ref(roastedduck_ast::TableRefKind::TableFunction(
            roastedduck_ast::TableFunctionRef {
                function: Box::new(function),
                column_name_alias: Vec::new(),
                with_ordinality: roastedduck_ast::OrdinalityType::WithoutOrdinality,
            },
        ));
        table.alias.clear();
        crate::relation::PyRelation::from_table_function(slf, table)
    }
    #[pyo3(signature=(format="json"))]
    fn get_profiling_information(&self, py: Python<'_>, format: &str) -> PyResult<Py<PyAny>> {
        if ![
            "query_tree",
            "json",
            "query_tree_optimizer",
            "no_output",
            "html",
            "graphviz",
        ]
        .contains(&format)
        {
            return Err(error::native(
                py,
                &Error::invalid_input(format!(
                    "Invalid ProfilerPrintFormat string: {format}. Valid options are: query_tree, json, query_tree_optimizer, no_output, html, graphviz."
                )),
            ));
        }
        let text = py
            .detach(|| lock(&self.state).connection()?.profiling_information(format))
            .map_err(|e| error::native(py, &e))?;
        conversion::text(py, &text)
    }
    fn enable_profiling(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            lock(&self.state).connection()?.enable_profiling();
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))
    }
    fn disable_profiling(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            lock(&self.state).connection()?.disable_profiling();
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn array_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("array_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn decimal_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("decimal_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn dtype(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("dtype", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn enum_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("enum_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn row_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("row_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn sqltype(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("sqltype", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn string_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("string_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn union_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("union_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn list_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("list_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn map_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("map_type", Some(slf), args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn struct_type(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("struct_type", Some(slf), args, kwargs)
    }
    #[pyo3(name="type", signature=(*args, **kwargs))]
    fn type_object(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<PyType> {
        crate::type_creation::call("type", Some(slf), args, kwargs)
    }
    fn from_arrow(
        slf: Bound<'_, Self>,
        arrow_object: &Bound<'_, PyAny>,
    ) -> PyResult<crate::relation::PyRelation> {
        self_check_relation(&slf, arrow_object, "arrow_object")?;
        let table = crate::input::arrow_table(arrow_object)?;
        crate::relation::PyRelation::from_table_ref(slf, table)
    }
    fn from_df(slf: Bound<'_, Self>, df: &Bound<'_, PyAny>) -> PyResult<crate::relation::PyRelation> {
        slf.get().check_client_open(slf.py())?;
        if !crate::pandas::is_dataframe(df)? {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "from_df(): incompatible function arguments",
            ));
        }
        let table = crate::pandas::table(df)?;
        crate::relation::PyRelation::from_table_ref(slf, table)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn create_function<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, Self>> {
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyConnection, name: str, function: collections.abc.Callable, parameters: object = None, return_type: _duckdb._sqltypes.DuckDBPyType = None, *, type: _duckdb._func.PythonUDFType = <PythonUDFType.NATIVE: 0>, null_handling: _duckdb._func.FunctionNullHandling = <FunctionNullHandling.DEFAULT: 0>, exception_handling: _duckdb.PythonExceptionHandling = <PythonExceptionHandling.DEFAULT: 0>, side_effects: bool = False) -> _duckdb.DuckDBPyConnection";
        let bad = || error::arguments(py, "create_function", signature, Some(slf.as_any()), args, kwargs);
        let values = crate::arguments::fields(
            "create_function",
            signature,
            slf.as_any(),
            args,
            kwargs,
            &[
                "name",
                "function",
                "parameters",
                "return_type",
                "type",
                "null_handling",
                "exception_handling",
                "side_effects",
            ],
            2,
            4,
        )?;
        let value = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated UDF name"))?;
        let Some(name) = crate::arguments::string_bytes(value)? else {
            return Err(bad()?);
        };
        let name = ByteString::from(name);
        let function = values[1]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated callable"))?;
        if !function.is_callable() {
            return Err(bad()?);
        }
        let optional = |index: usize| values[index].as_ref().filter(|value| !value.is_none());
        let parameters = optional(2);
        let return_type = optional(3);
        let r#type = optional(4);
        let null_handling = optional(5);
        let exception_handling = optional(6);
        let side_effects = match values[7].as_ref() {
            Some(value) => match value.extract::<bool>() {
                Ok(value) => value,
                Err(_) => return Err(bad()?),
            },
            None => false,
        };
        let state = Arc::clone(&slf.get().state);
        let arrow = crate::udf::option(r#type, "udf_type", ["native", "arrow"])?;
        let special = crate::udf::option(null_handling, "null_handling", ["default", "special"])?;
        let return_null = crate::udf::option(
            exception_handling,
            "exception_handling",
            ["default", "return_null"],
        )?;
        let properties=py.detach(|| {
            let state=lock(&state); let mut connection=state.connection()?;
            connection.cancel_registration_transaction()?;
            if state.functions.contains(&name.to_ascii_lowercase()) {return Err(Error::not_implemented(roastedduck_common::byte_format!("A function by the name of '{name}' is already created, creating multiple functions with the same name is not supported yet, please remove it first")));}
            Ok::<_,Error>(connection.client_properties())
        }).map_err(|e|error::native(py,&e))?;
        let set = crate::udf::function(
            py,
            name.as_bytes(),
            function,
            parameters,
            return_type,
            arrow,
            special,
            return_null,
            side_effects,
            properties,
        )?;
        py.detach(|| {
            let mut state = lock(&state);
            state.connection()?.register_scalar_function_set(set)?;
            state.functions.insert(name.to_ascii_lowercase());
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))?;
        Ok(slf)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn remove_function<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, Self>> {
        let py = slf.py();
        let signature = "(self: _duckdb.DuckDBPyConnection, name: str) -> _duckdb.DuckDBPyConnection";
        let values = crate::arguments::fields(
            "remove_function",
            signature,
            slf.as_any(),
            args,
            kwargs,
            &["name"],
            1,
            1,
        )?;
        let value = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated UDF name"))?;
        let Some(name) = crate::arguments::string_bytes(value)? else {
            return Err(error::arguments(
                py,
                "remove_function",
                signature,
                Some(slf.as_any()),
                args,
                kwargs,
            )?);
        };
        let name = ByteString::from(name);
        let state = Arc::clone(&slf.get().state);
        py.detach(|| {
            let mut state = lock(&state);
            if !state.functions.contains(&name.to_ascii_lowercase()) {
                return Err(Error::invalid_input(roastedduck_common::byte_format!(
                    "No function by the name of '{name}' was found in the list of registered functions"
                )));
            }
            state.connection()?.remove_scalar_function(name.as_bytes())?;
            state.functions.remove(&name.to_ascii_lowercase());
            Ok::<_, Error>(())
        })
        .map_err(|error| error::native(py, &error))?;
        Ok(slf)
    }
    #[pyo3(signature=(*args, **kwargs))]
    pub(super) fn register<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, Self>> {
        let signature = "(self: _duckdb.DuckDBPyConnection, view_name: str, python_object: object) -> _duckdb.DuckDBPyConnection";
        let values = crate::arguments::fields(
            "register",
            signature,
            slf.as_any(),
            args,
            kwargs,
            &["view_name", "python_object"],
            2,
            2,
        )?;
        let name = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated view name"))?;
        let Some(view_name) = crate::arguments::string_bytes(name)? else {
            return Err(error::arguments(
                slf.py(),
                "register",
                signature,
                Some(slf.as_any()),
                args,
                kwargs,
            )?);
        };
        let python_object = values[1]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated Python object"))?;
        self_check_relation(&slf, python_object, &view_name)?;
        let table = crate::input::table_named(python_object, &view_name)?;
        let py = slf.py();
        let state = Arc::clone(&slf.get().state);
        py.detach(|| {
            let mut state = lock(&state);
            let replace = state.registered.contains(view_name.as_slice());
            crate::input::create_view(
                &mut *state.connection()?,
                crate::input::select(table),
                &view_name,
                replace,
                true,
            )?;
            state.registered.insert(view_name.clone().into());
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))?;
        Ok(slf)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn append<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, pyo3::types::PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, Self>> {
        crate::append::call(slf, args, kwargs)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn unregister<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, Self>> {
        let signature = "(self: _duckdb.DuckDBPyConnection, view_name: str) -> _duckdb.DuckDBPyConnection";
        let values = crate::arguments::fields(
            "unregister",
            signature,
            slf.as_any(),
            args,
            kwargs,
            &["view_name"],
            1,
            1,
        )?;
        let name = values[0]
            .as_ref()
            .ok_or_else(|| error::InternalException::new_err("Missing validated view name"))?;
        let Some(view_name) = crate::arguments::string_bytes(name)? else {
            return Err(error::arguments(
                slf.py(),
                "unregister",
                signature,
                Some(slf.as_any()),
                args,
                kwargs,
            )?);
        };
        let py = slf.py();
        let state = Arc::clone(&slf.get().state);
        py.detach(|| {
            let mut state = lock(&state);
            drop(state.connection()?);
            if state.registered.remove(view_name.as_slice()) {
                let quoted = roastedduck_ast::keyword::write_optionally_quoted_bytes(&view_name);
                let _ = state
                    .connection()?
                    .query(roastedduck_common::byte_format!("DROP VIEW {quoted}"));
            }
            Ok::<_, Error>(())
        })
        .map_err(|e| error::native(py, &e))?;
        Ok(slf)
    }
    #[pyo3(signature=(*args, **kwargs))]
    fn view<'py>(
        slf: Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<crate::relation::PyRelation> {
        Self::named_relation(slf, "view", args, kwargs)
    }
    fn fetchone(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (row, timezone, checkpoint) = py
            .detach(|| lock(&self.state).next_with_timezone())
            .map_err(|e| error::native(py, &e))?;
        match row {
            Some(row) => self.row_to_python(py, &row, &timezone, checkpoint),
            None => Ok(py.None()),
        }
    }
    fn fetchall(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.fetch_rows(py, None)
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
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, rows_per_batch=1000000, *, lazy=False)")]
    fn pl(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let (rows_per_batch, lazy) = crate::polars::arguments(slf.as_any(), args, kwargs, false)?;
        let this = slf.get();
        if !lazy {
            let table = if rows_per_batch == 0 {
                let mut metadata = py
                    .detach(|| lock(&this.state).host_metadata())
                    .map_err(|e| error::native(py, &e))?;
                metadata.names = crate::host::names(&metadata.names);
                py.detach(|| {
                    while lock(&this.state).host_chunk()?.is_some() {}
                    Ok::<_, Error>(())
                })
                .map_err(|e| error::native(py, &e))?;
                crate::host::arrow(py, &metadata, 1_000_000, || Ok(None))?
            } else {
                this.fetch_arrow_named(py, rows_per_batch, true)?
            };
            return crate::polars::from_arrow(py, table);
        }
        let (connection, result) = py
            .detach(|| {
                let mut state = lock(&this.state);
                let result = state
                    .result
                    .take()
                    .ok_or_else(|| Error::invalid_input("No open result set"))?;
                state.transferred = Some((result.names().to_vec(), result.types().to_vec()));
                Ok::<_, Error>((state.reader_handle()?, result))
            })
            .map_err(|e| error::native(py, &e))?;
        let reader = crate::reader::export_polars(py, connection, result)?;
        crate::polars::from_reader(py, reader)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, batch_size=1000000)")]
    fn to_arrow_reader(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let batch_size =
            crate::reader::batch_arguments(slf.as_any(), args, kwargs, "to_arrow_reader", false)?;
        slf.get().arrow_reader(py, batch_size, false)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, rows_per_batch=1000000)")]
    fn arrow(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let rows_per_batch = crate::reader::batch_arguments(slf.as_any(), args, kwargs, "arrow", false)?;
        slf.get().arrow_reader(py, rows_per_batch, false)
    }
    #[pyo3(signature=(*args,**kwargs),text_signature="($self, rows_per_batch=1000000)")]
    fn fetch_record_batch(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let rows_per_batch =
            crate::reader::batch_arguments(slf.as_any(), args, kwargs, "fetch_record_batch", false)?;
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
    #[pyo3(signature=(rows_per_batch=1_000_000))]
    fn to_arrow_table(&self, py: Python<'_>, rows_per_batch: usize) -> PyResult<Py<PyAny>> {
        self.fetch_arrow(py, rows_per_batch)
    }
    #[pyo3(signature=(rows_per_batch=1_000_000))]
    fn fetch_arrow_table(&self, py: Python<'_>, rows_per_batch: usize) -> PyResult<Py<PyAny>> {
        error::deprecated(py, "fetch_arrow_table", "to_arrow_table")?;
        self.fetch_arrow(py, rows_per_batch)
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
    #[pyo3(signature=(size=1))]
    fn fetchmany(&self, py: Python<'_>, size: usize) -> PyResult<Py<PyAny>> {
        self.fetch_rows(py, Some(size))
    }
    fn __del__(&self, py: Python<'_>) {
        self.close(py);
    }
    fn close(&self, py: Python<'_>) {
        py.detach(|| {
            let mut pending = vec![Arc::clone(&self.state)];
            while let Some(state) = pending.pop() {
                let mut state = lock(&state);
                pending.extend(state.children.drain(..).filter_map(|child| child.upgrade()));
                state.clear_result();
                state.functions.clear();
                state.connection = None;
                state.closed.store(true, Ordering::Release);
                state.database = None;
            }
        });
    }
    #[getter]
    fn _closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    fn cursor(&self, py: Python<'_>) -> PyResult<Self> {
        let child = py
            .detach(|| {
                let mut state = lock(&self.state);
                let database = state.database.clone().ok_or_else(|| {
                    Error::new(
                        roastedduck_common::ErrorKind::Connection,
                        "Connection already closed!",
                    )
                })?;
                let child = Self::from_database(database)?;
                state.children.retain(|child| child.strong_count() > 0);
                state.children.push(Arc::downgrade(&child.state));
                Ok::<_, Error>(child)
            })
            .map_err(|e| error::native(py, &e))?;
        Ok(child)
    }
    fn duplicate(&self, py: Python<'_>) -> PyResult<Self> {
        self.cursor(py)
    }
    fn interrupt(&self, py: Python<'_>) -> PyResult<()> {
        self.check_client_open(py)?;
        self.client.interrupt();
        Ok(())
    }
    fn query_progress(&self, py: Python<'_>) -> PyResult<f64> {
        self.check_client_open(py)?;
        Ok(self.client.query_progress().percentage)
    }
    fn begin(slf: Bound<'_, Self>) -> PyResult<Bound<'_, Self>> {
        Self::command(slf, "BEGIN TRANSACTION")
    }
    fn commit(slf: Bound<'_, Self>) -> PyResult<Bound<'_, Self>> {
        let py = slf.py();
        let state = &slf.get().state;
        let auto = py
            .detach(|| lock(state).connection().map(|c| c.is_auto_commit()))
            .map_err(|e| error::native(py, &e))?;
        if auto {
            Ok(slf)
        } else {
            Self::command(slf, "COMMIT")
        }
    }
    fn rollback(slf: Bound<'_, Self>) -> PyResult<Bound<'_, Self>> {
        Self::command(slf, "ROLLBACK")
    }
    fn checkpoint(slf: Bound<'_, Self>) -> PyResult<Bound<'_, Self>> {
        Self::command(slf, "CHECKPOINT")
    }
    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }
    fn __exit__(
        &self,
        py: Python<'_>,
        _ty: &Bound<'_, PyAny>,
        _value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) {
        self.close(py);
    }
    fn __iter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let row = self.fetchone(py)?;
        if row.is_none(py) { Ok(None) } else { Ok(Some(row)) }
    }
    #[getter]
    #[allow(clippy::unused_self)] // Python property; the frozen API always reports unknown.
    fn rowcount(&self) -> i64 {
        -1
    }
    #[getter]
    fn description(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let metadata = py.detach(|| {
            let state = lock(&self.state);
            state
                .result
                .as_ref()
                .map(|result| (result.names().to_vec(), result.types().to_vec()))
                .or_else(|| state.transferred.clone())
        });
        let Some((names, types)) = metadata else {
            return Ok(py.None());
        };
        let list = PyList::empty(py);
        for (name, kind) in names.iter().zip(&types) {
            let row = vec![
                conversion::text(py, name)?,
                Py::new(py, PyType { native: kind.clone() })?.into_any(),
                py.None(),
                py.None(),
                py.None(),
                py.None(),
                py.None(),
            ];
            list.append(PyTuple::new(py, row)?)?;
        }
        Ok(list.unbind().into_any())
    }
}
#[pyfunction]
#[pyo3(signature=(*args,**kwargs))]
pub fn connect<'py>(
    py: Python<'py>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Py<PyConnection>> {
    let bad = || {
        error::arguments(
            py,
            "connect",
            "(database: object = ':memory:', read_only: bool = False, config: dict = None) -> duckdb.DuckDBPyConnection",
            None,
            args,
            kwargs,
        )
    };
    if args.len()>3 || kwargs.is_some_and(|kwargs|kwargs.iter().any(|(key,_)|!matches!(key.extract::<String>(),Ok(key)if matches!(key.as_str(),"database"|"read_only"|"config")))){return Err(bad()?);}
    let field = |index, name| -> PyResult<Option<Bound<'py, PyAny>>> {
        let keyword = kwargs.map(|kw| kw.get_item(name)).transpose()?.flatten();
        if args.len() > index {
            if keyword.is_some() {
                return Err(bad()?);
            }
            args.get_item(index).map(Some)
        } else {
            Ok(keyword)
        }
    };
    let database =
        field(0, "database")?.unwrap_or_else(|| pyo3::types::PyString::new(py, ":memory:").into_any());
    let read_only = if let Some(flag) = field(1, "read_only")? {
        if flag.is_none() {
            false
        } else {
            // SAFETY: the object is live and the caller holds the GIL.
            if unsafe { pyo3::ffi::PyNumber_Check(flag.as_ptr()) } == 0 {
                return Err(bad()?);
            }
            flag.is_truthy()?
        }
    } else {
        false
    };
    let config = field(2, "config")?.filter(|v| !v.is_none());
    let config = if let Some(config) = config {
        Some(
            config
                .cast_into::<PyDict>()
                .map_err(|_| bad().unwrap_or_else(|e| e))?,
        )
    } else {
        None
    };
    let config = config.as_ref();
    if !database.is_instance_of::<pyo3::types::PyString>()
        && !database.is_instance(&py.import("pathlib")?.getattr("Path")?)?
    {
        return Err(error::native(
            py,
            &Error::invalid_input(format!(
                "Please provide either a str or a pathlib.Path, not {}",
                database.get_type().str()?
            )),
        ));
    }
    let text = database.str()?.extract::<String>()?;
    if text.eq_ignore_ascii_case(":default:") {
        if read_only || config.is_some_and(|config| !config.is_empty()) {
            return Err(error::native(
                py,
                &Error::invalid_input(
                    "Default connection fetching is only allowed without additional options",
                ),
            ));
        }
        return Ok(py
            .import("duckdb")?
            .getattr("default_connection")?
            .call0()?
            .cast_into::<PyConnection>()?
            .unbind());
    }
    let path = PathBuf::from(text);
    let mut settings = Config::default();
    if read_only {
        settings.access_mode = AccessMode::ReadOnly;
    }
    let mut extras = vec![
        ("pandas_analyze_sample".to_owned(), Value::ubigint(1000)),
        ("python_enable_replacements".to_owned(), Value::boolean(true)),
        ("python_scan_all_frames".to_owned(), Value::boolean(false)),
    ];
    if let Some(config) = config {
        for (key, value) in config.iter() {
            let key = key.extract::<String>()?;
            let value = conversion::from_python(&value)?;
            if let Some((_, target)) = extras
                .iter_mut()
                .find(|(name, _)| name.eq_ignore_ascii_case(&key))
            {
                use roastedduck_function::cast::ValueCast;
                *target = value
                    .default_cast_as(target.logical_type())
                    .map_err(|e| error::native(py, &e))?;
            } else {
                settings
                    .set_option_by_name(&key, value)
                    .map_err(|e| error::native(py, &e))?;
            }
        }
    }
    let version = py.import("sys")?.getattr("version_info")?;
    let major = version.getattr("major")?.extract::<u8>()?;
    let minor = version.getattr("minor")?.extract::<u8>()?;
    settings
        .set_option_by_name("duckdb_api", Value::varchar(format!("python/{major}.{minor}")))
        .map_err(|e| error::native(py, &e))?;
    settings.options.sort_by(|a, b| a.0.cmp(&b.0));
    let database = py
        .detach(|| open_database(&path, &settings, &extras))
        .map_err(|e| error::native(py, &e))?;
    let connection = py
        .detach(|| PyConnection::from_database(database))
        .map_err(|e| error::native(py, &e))?;
    Py::new(py, connection)
}
