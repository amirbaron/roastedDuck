//! SQL/file-system adapters for roastedDuck's PostgreSQL binary COPY format.
use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_function::copy::CopyGlobalState;
use roastedduck_function::registered_files::Output;
use roastedduck_function::{
    CopyFunction, FunctionClient, FunctionData, TableFunction, TableFunctionBindOutput, TableFunctionSet,
};
use roastedduck_postgres::binary_file::{DEFAULT_BUFFER_SIZE, Reader};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId};
use std::any::Any;
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug)]
struct ReadData {
    path: ByteString,
    names: Vec<ByteString>,
    types: Vec<LogicalType>,
    buffer_size: usize,
    client: Arc<FunctionClient>,
}
impl FunctionData for ReadData {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.path == other.path && self.names == other.names)
    }
}
#[derive(Debug)]
struct WriteData {
    client: Arc<FunctionClient>,
}
impl FunctionData for WriteData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct LoggedOutput {
    file: Output,
    logger: Option<roastedduck_function::logging::FileSystemLogger>,
    position: u64,
}
impl Write for LoggedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes)?;
        if let Some(logger) = &self.logger {
            logger.write(written as u64, self.position);
        }
        self.position += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
impl Drop for LoggedOutput {
    fn drop(&mut self) {
        if let Some(logger) = &self.logger {
            logger.close();
        }
    }
}
struct Writer {
    file: Mutex<Option<BufWriter<LoggedOutput>>>,
    path: ByteString,
    replacement: Option<Vec<u8>>,
}
fn write_error(path: &[u8], error: &std::io::Error) -> Error {
    roastedduck_function::registered_files::callback_error(error).unwrap_or_else(|| {
        Error::new_bytes(
            ErrorKind::Io,
            roastedduck_common::byte_format!("Could not write file \"{path}\": {error}"),
        )
    })
}
fn writer(state: &CopyGlobalState) -> Result<&Writer> {
    state
        .downcast_ref::<Writer>()
        .ok_or_else(|| Error::internal("Missing Postgres binary writer"))
}
fn client(binder: Option<&dyn Any>) -> Result<Arc<FunctionClient>> {
    binder
        .and_then(|v| v.downcast_ref::<roastedduck_planner::ClientContext>())
        .map(|c| c.function_client.clone())
        .or_else(roastedduck_function::file_system::opener)
        .ok_or_else(|| Error::internal("Postgres binary COPY requires a client context"))
}

pub(crate) fn copy_function() -> CopyFunction {
    let mut function = CopyFunction::new("postgres_binary");
    function.copy_to_bind = Some(Arc::new(|input, _, _| {
        Ok(Arc::new(WriteData {
            client: client(input.binder)?,
        }))
    }));
    function.copy_to_initialize_global = Some(Arc::new(|data, path| {
        let data = data
            .as_any()
            .downcast_ref::<WriteData>()
            .ok_or_else(|| Error::internal("Missing Postgres binary write data"))?;
        let _opener = roastedduck_function::file_system::activate_opener(data.client.clone());
        let replacement = data
            .client
            .settings()
            .and_then(|s| s.current_setting("pg_null_byte_replacement").ok())
            .filter(|v| !v.is_null())
            .map(|v| v.to_varchar_bytes().map(|v| v.to_vec()))
            .transpose()?;
        if replacement.as_ref().is_some_and(|bytes| bytes.contains(&0)) {
            return Err(Error::internal(
                "NULL byte replacement string cannot contain NULL values",
            ));
        }
        roastedduck_function::file_system::verify_can_write(path)?;
        let file = if roastedduck_function::registered_files::lookup(path).is_some() {
            Output::create(path)?
        } else {
            Output::Local(
                std::fs::File::create(roastedduck_function::file_system::native_path(path)?)
                    .map_err(|e| roastedduck_csv::buffer::open_error(path, &e))?,
            )
        };
        let mut file = BufWriter::with_capacity(
            32 * 1024,
            LoggedOutput {
                file,
                logger: roastedduck_function::logging::FileSystemLogger::open_for(Some(&data.client), path),
                position: 0,
            },
        );
        file.write_all(roastedduck_postgres::copy::BINARY_HEADER)
            .map_err(|e| write_error(path, &e))?;
        Ok(Box::new(Writer {
            file: Mutex::new(Some(file)),
            path: path.into(),
            replacement,
        }) as CopyGlobalState)
    }));
    function.copy_to_sink = Some(Arc::new(|_, state, chunk| {
        let writer = writer(state)?;
        let mut guard = writer.file.lock().unwrap_or_else(PoisonError::into_inner);
        let file = guard
            .as_mut()
            .ok_or_else(|| Error::internal("Postgres binary writer is closed"))?;
        for row in 0..chunk.len() {
            let values = chunk
                .columns()
                .iter()
                .map(|c| c.get_value(row))
                .collect::<Vec<_>>();
            let bytes =
                roastedduck_postgres::binary_file::encode_row(&values, writer.replacement.as_deref())?;
            file.write_all(&bytes)
                .map_err(|e| write_error(&writer.path, &e))?;
        }
        Ok(())
    }));
    function.copy_to_finalize = Some(Arc::new(|_, state| {
        let writer = writer(state)?;
        if let Some(mut file) = writer.file.lock().unwrap_or_else(PoisonError::into_inner).take() {
            file.write_all(roastedduck_postgres::copy::BINARY_FOOTER)
                .map_err(|e| write_error(&writer.path, &e))?;
            file.flush().map_err(|e| write_error(&writer.path, &e))?;
            file.get_mut()
                .file
                .close()
                .map_err(|e| write_error(&writer.path, &e))?;
        }
        Ok(())
    }));
    function.copy_from_bind = Some(Arc::new(|input, names, types| {
        Ok(Arc::new(ReadData {
            path: input.file_path.into(),
            names: names.to_vec(),
            types: types.to_vec(),
            buffer_size: DEFAULT_BUFFER_SIZE,
            client: client(input.binder)?,
        }))
    }));
    function.copy_from_function = Some(reader_function());
    function
}

fn reader_function() -> TableFunction {
    let mut function = TableFunction::new("read_postgres_binary",vec![LogicalType::VARCHAR]).with_bind(|input| {
        let context = input.binder.and_then(|c| c.downcast_ref::<roastedduck_planner::ClientContext>())
            .ok_or_else(|| Error::internal("Postgres binary read requires a client context"))?;
        let named = |name: &str| input.named_parameters.iter().find(|(n,_)| n.eq_ignore_ascii_case(name.as_bytes())).map(|(_,v)|v);
        let columns = named("columns").ok_or_else(|| Error::binder("read_postgres_binary requires a 'columns' parameter, e.g. columns={col1: 'INTEGER', col2: 'VARCHAR'}"))?;
        if columns.logical_type().id()!=LogicalTypeId::Struct { return Err(Error::internal("Calling StructValue::GetChildren on a non-STRUCT")); }
        let mut names = Vec::new(); let mut types = Vec::new();
        for ((name,_),value) in columns.logical_type().struct_children().unwrap_or_default().iter().zip(columns.children().unwrap_or_default()) {
            names.push(name.clone());
            types.push(roastedduck_csv::read_csv::transform_string_to_logical_type(context,&value.to_varchar()?)?);
        }
        let buffer_size = named("buffer_size").map_or(Ok(DEFAULT_BUFFER_SIZE),|v| {
            v.as_u64().and_then(|v| usize::try_from(v).ok()).ok_or_else(|| Error::invalid_input("Invalid Postgres binary buffer size"))
        })?;
        Ok(TableFunctionBindOutput { names: names.clone(), return_types: types.clone(), bind_data: Some(Arc::new(ReadData {
            path: input.inputs[0].to_varchar_bytes()?, names, types, buffer_size, client: context.function_client.clone(),
        })) })
    }).with_scan(|data,_,state,output| {
        let data = data.and_then(|d| d.as_any().downcast_ref::<ReadData>()).ok_or_else(|| Error::internal("Missing Postgres binary read data"))?;
        let _opener = roastedduck_function::file_system::activate_opener(data.client.clone());
        if state.local.is_none() {
            let options = roastedduck_csv::options::CsvReaderOptions {
                compression: roastedduck_csv::options::FileCompressionType::Uncompressed, ..Default::default()
            };
            let path = roastedduck_function::file_system::expand_path_bytes(&data.path);
            let mut file = roastedduck_csv::buffer::CsvFileHandle::open(&path,&options)?;
            let size = file.file_size();
            let config = super::catalog::type_options(&data.client);
            let codecs = data.types.iter().map(|ty| roastedduck_postgres::codec::Codec::for_file(ty,config)).collect::<Result<_>>()?;
            let reader = Reader::new(&data.path,size,data.buffer_size,codecs,Box::new(move |bytes| file.read(bytes)))?;
            state.local = Some(Box::new(reader));
        }
        let reader = state.local.as_mut().and_then(|v| v.downcast_mut::<Reader>()).ok_or_else(|| Error::internal("Invalid Postgres binary reader"))?;
        let rows = reader.next_batch()?;
        *output = DataChunk::with_types(&data.types,rows.len().max(1));
        for (row,values) in rows.iter().enumerate() { for (column,value) in values.iter().enumerate() { output.column_mut(column).set_value(row,value)?; } }
        output.set_len(rows.len()); state.position += rows.len() as u64;
        Ok(!rows.is_empty())
    });
    function
        .named_parameters
        .push(("columns".into(), LogicalType::ANY));
    function
        .named_parameters
        .push(("buffer_size".into(), LogicalType::UBIGINT));
    function.serialize = Some(Arc::new(|serializer, data, _| {
        let data = data
            .and_then(|d| d.as_any().downcast_ref::<ReadData>())
            .ok_or_else(|| Error::internal("Missing Postgres binary read data"))?;
        serializer.write_property(100, "file_path", &data.path)?;
        serializer.write_property(101, "names", &data.names)?;
        serializer.write_property(102, "types", &data.types)?;
        serializer.write_property(103, "buffer_size", &(data.buffer_size as u64))
    }));
    function
}
pub(crate) fn table_function() -> TableFunctionSet {
    let mut set = TableFunctionSet::new("read_postgres_binary");
    set.functions.push(reader_function());
    set
}
