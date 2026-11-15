//! A PostgreSQL session owned by one engine transaction, with bounded result storage.
use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use roastedduck_buffer::{BufferManager, MemoryTag};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_function::FunctionClient;
use roastedduck_function::cast::ValueCast;
use roastedduck_postgres::{
    BinaryParameter, Column, Rows, Session,
    codec::{Codec, Options},
};
use roastedduck_row::{ColumnDataAppendState, ColumnDataCollection, ColumnDataScanState};
use roastedduck_storage::transaction::TransactionParticipant;
use roastedduck_types::{DataChunk, LogicalType, Value};

#[derive(Clone, Debug)]
pub(crate) enum Decoder {
    Value(Codec),
    Text(LogicalType),
    Ctid,
    TextCtid,
}
impl Decoder {
    fn logical_type(&self) -> LogicalType {
        match self {
            Self::Value(codec) => codec.logical_type.clone(),
            Self::Text(ty) => ty.clone(),
            Self::Ctid | Self::TextCtid => LogicalType::BIGINT,
        }
    }
    fn decode(&self, value: Option<&[u8]>) -> Result<Value> {
        match self {
            Self::Value(codec) => codec.decode(value),
            Self::Text(ty) => super::text::decode(ty, value),
            Self::TextCtid => super::text::ctid(value),
            Self::Ctid => {
                let Some(bytes) = value else {
                    return Ok(Value::null(LogicalType::BIGINT));
                };
                let bytes: &[u8; 6] = bytes
                    .try_into()
                    .map_err(|_| Error::invalid_input("Invalid Postgres CTID length"))?;
                let page = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let row = i16::from_be_bytes([bytes[4], bytes[5]]);
                Ok(Value::bigint((i64::from(page) << 16) + i64::from(row)))
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Cursor {
    source: Option<Rows>,
    codecs: Vec<Decoder>,
    client: Arc<FunctionClient>,
    buffered: Option<(ColumnDataCollection, ColumnDataScanState)>,
}
impl Cursor {
    fn read_wire(&mut self) -> Result<Option<DataChunk>> {
        let Some(rows) = &mut self.source else {
            return Ok(None);
        };
        let batch =
            rows.next_batch_interruptible(STANDARD_VECTOR_SIZE, self.client.interrupt_flag().as_deref())?;
        if batch.len() < STANDARD_VECTOR_SIZE {
            self.source = None;
        }
        if batch.is_empty() {
            return Ok(None);
        }
        let types = self.codecs.iter().map(Decoder::logical_type).collect::<Vec<_>>();
        let mut chunk = DataChunk::with_types(&types, batch.len());
        for (row, values) in batch.iter().enumerate() {
            for (column, codec) in self.codecs.iter().enumerate() {
                let value = codec.decode(values[column].as_deref())?;
                chunk.column_mut(column).set_value(row, &value)?;
            }
        }
        chunk.set_len(batch.len());
        Ok(Some(chunk))
    }
    pub fn next(&mut self) -> Result<Option<DataChunk>> {
        if let Some((collection, state)) = &mut self.buffered {
            let result = collection.scan(state)?;
            if result.is_none() {
                self.buffered = None;
            }
            Ok(result)
        } else {
            self.read_wire()
        }
    }
    fn materialize(&mut self, manager: &BufferManager) -> Result<()> {
        if self.source.is_none() {
            return Ok(());
        }
        let types = self.codecs.iter().map(Decoder::logical_type).collect();
        let mut collection = ColumnDataCollection::new(manager.clone(), types, MemoryTag::ColumnData)?;
        let mut append = ColumnDataAppendState::default();
        while let Some(chunk) = self.read_wire()? {
            collection.append(&mut append, &chunk)?;
        }
        let state = collection.initialize_scan();
        self.buffered = Some((collection, state));
        Ok(())
    }
    fn close(&mut self) {
        self.source = None;
        self.buffered = None;
    }
}

#[derive(Debug, Default)]
struct State {
    started: bool,
    finished: bool,
    cursors: Vec<Weak<Mutex<Cursor>>>,
}

#[derive(Debug)]
pub(crate) struct PgTransaction {
    session: Session,
    client: Arc<FunctionClient>,
    buffer_manager: BufferManager,
    begin: String,
    read_only: bool,
    state: Mutex<State>,
}
impl PgTransaction {
    pub fn copy_input(
        &self,
        sql: &str,
        text: bool,
        chunks: &mut dyn Iterator<Item = Result<DataChunk>>,
    ) -> Result<u64> {
        let Some(first) = chunks.next() else {
            return Ok(0);
        };
        let first = first?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, true)?;
        let replacement = self
            .client
            .settings()
            .and_then(|s| s.current_setting("pg_null_byte_replacement").ok())
            .and_then(|v| if v.is_null() { None } else { Some(v.to_varchar()) })
            .transpose()?;
        let mut writer = self.session.copy_in(sql)?;
        let interrupted = self.client.interrupt_flag();
        let mut count = 0;
        if !text {
            writer.write_interruptible(
                roastedduck_postgres::copy::BINARY_HEADER.to_vec().into(),
                interrupted.as_deref(),
            )?;
        }
        for chunk in std::iter::once(Ok(first)).chain(chunks) {
            let chunk = chunk?;
            count += chunk.len() as u64;
            let types = chunk
                .columns()
                .iter()
                .map(|v| roastedduck_postgres::copy::postgres_type(v.logical_type()))
                .collect::<Vec<_>>();
            let mut bytes = Vec::new();
            for row in 0..chunk.len() {
                let values = chunk
                    .columns()
                    .iter()
                    .zip(&types)
                    .map(|(column, ty)| column.get_value(row).default_cast_as(ty))
                    .collect::<Result<Vec<_>>>()?;
                bytes.extend_from_slice(&if text {
                    roastedduck_postgres::copy::text_row(&values, replacement.as_deref())?
                } else {
                    roastedduck_postgres::copy::binary_row(&values, replacement.as_deref())?
                });
            }
            writer.write_interruptible(bytes.into(), interrupted.as_deref())?;
        }
        writer.write_interruptible(
            if text {
                roastedduck_postgres::copy::TEXT_FOOTER
            } else {
                roastedduck_postgres::copy::BINARY_FOOTER
            }
            .to_vec()
            .into(),
            interrupted.as_deref(),
        )?;
        writer.finish_interruptible(interrupted.as_deref())?;
        Ok(count)
    }
    pub fn new(
        session: Session,
        client: Arc<FunctionClient>,
        buffer_manager: BufferManager,
        isolation: &str,
        read_only: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            session,
            client,
            buffer_manager,
            read_only,
            begin: format!(
                "BEGIN TRANSACTION ISOLATION LEVEL {isolation}{}",
                if read_only { " READ ONLY" } else { "" }
            ),
            state: Mutex::default(),
        })
    }
    fn prepare(&self, state: &mut State, use_transaction: bool) -> Result<()> {
        if state.finished {
            return Err(Error::internal("Postgres transaction already finished"));
        }
        for cursor in state.cursors.iter().filter_map(Weak::upgrade) {
            cursor
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .materialize(&self.buffer_manager)?;
        }
        state.cursors.retain(|cursor| cursor.strong_count() > 0);
        if use_transaction {
            if !state.started {
                self.session
                    .batch_interruptible(&self.begin, self.client.interrupt_flag().as_deref())?;
                state.started = true;
            }
        } else if state.started {
            return Err(Error::invalid_input(
                "Execution without a Transaction is not possible if a Transaction already started",
            ));
        } else if self.read_only {
            return Err(Error::invalid_input(
                "Execution without a Transaction is not possible in Read Only Mode",
            ));
        }
        Ok(())
    }
    pub fn describe(&self, sql: &str, use_transaction: bool) -> Result<Vec<Column>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, use_transaction)?;
        self.session
            .describe_interruptible(sql, self.client.interrupt_flag().as_deref())
    }
    pub fn describe_query(
        &self,
        sql: &str,
        use_transaction: bool,
    ) -> Result<roastedduck_postgres::Description> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, use_transaction)?;
        self.session
            .describe_query(sql, self.client.interrupt_flag().as_deref())
    }
    pub fn execute(&self, sql: &str, use_transaction: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, use_transaction)?;
        self.session
            .batch_interruptible(sql, self.client.interrupt_flag().as_deref())
    }
    /// Small catalog queries are consumed under the session lock. Data scans use
    /// cursors so their result size is not charged to an unbounded metadata vector.
    pub fn metadata(&self, sql: &str, parameters: &[BinaryParameter]) -> Result<Vec<Vec<Value>>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, true)?;
        let mut rows =
            self.session
                .stream_interruptible(sql, parameters, self.client.interrupt_flag().as_deref())?;
        let codecs = rows
            .columns
            .iter()
            .map(|c| Codec::new(&c.data_type, c.type_modifier, 0, Options::default()))
            .collect::<Result<Vec<_>>>()?;
        let mut result = Vec::new();
        loop {
            let batch =
                rows.next_batch_interruptible(STANDARD_VECTOR_SIZE, self.client.interrupt_flag().as_deref())?;
            if batch.is_empty() {
                break;
            }
            for row in batch {
                result.push(
                    codecs
                        .iter()
                        .zip(row)
                        .map(|(codec, value)| codec.decode(value.as_deref()))
                        .collect::<Result<Vec<_>>>()?,
                );
            }
        }
        Ok(result)
    }
    pub fn cursor(
        &self,
        sql: &str,
        parameters: &[BinaryParameter],
        codecs: Vec<Decoder>,
        use_transaction: bool,
        text: bool,
    ) -> Result<Arc<Mutex<Cursor>>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, use_transaction)?;
        let rows = if text {
            self.session
                .stream_text(sql, parameters, self.client.interrupt_flag().as_deref())?
        } else {
            self.session
                .stream_interruptible(sql, parameters, self.client.interrupt_flag().as_deref())?
        };
        let cursor = Arc::new(Mutex::new(Cursor {
            source: Some(rows),
            codecs,
            client: self.client.clone(),
            buffered: None,
        }));
        state.cursors.push(Arc::downgrade(&cursor));
        Ok(cursor)
    }
    pub fn cursor_query(
        &self,
        sql: &str,
        parameters: &[roastedduck_postgres::QueryParameter],
        codecs: Vec<Decoder>,
        use_transaction: bool,
        text: bool,
    ) -> Result<Arc<Mutex<Cursor>>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.prepare(&mut state, use_transaction)?;
        let rows =
            self.session
                .stream_query(sql, parameters, text, self.client.interrupt_flag().as_deref())?;
        let cursor = Arc::new(Mutex::new(Cursor {
            source: Some(rows),
            codecs,
            client: self.client.clone(),
            buffered: None,
        }));
        state.cursors.push(Arc::downgrade(&cursor));
        Ok(cursor)
    }
    fn finish(&self, commit: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.finished {
            return Ok(());
        }
        state.finished = true;
        for cursor in state.cursors.drain(..).filter_map(|c| c.upgrade()) {
            cursor.lock().unwrap_or_else(PoisonError::into_inner).close();
        }
        if state.started {
            self.session.batch(if commit { "COMMIT" } else { "ROLLBACK" })?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Participant(pub Arc<PgTransaction>);
impl TransactionParticipant for Participant {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn commit(&self) -> Result<()> {
        self.0.finish(true)
    }
    fn rollback(&self) -> Result<()> {
        self.0.finish(false)
    }
}
