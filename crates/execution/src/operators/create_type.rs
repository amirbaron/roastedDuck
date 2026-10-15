//! `CREATE TYPE name AS ENUM (SELECT ...)` (DuckDB's `PhysicalCreateType`): the query's
//! distinct non-NULL values, in order of appearance, become the enum's members.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{CreateInfo, CreateInfoKind};
use rustdb_catalog::Catalog;
use rustdb_common::{Error, Result};
use rustdb_storage::Transaction;
use rustdb_types::{DataChunk, LogicalType};

use super::ddl::{CreateEntryAction, DdlAction};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkResult, Source, SourceResult};

/// The values collected so far (`CreateTypeGlobalState`).
#[derive(Debug, Default)]
pub struct CreateTypeGlobal {
    values: Mutex<(Vec<String>, HashSet<String>)>,
}

/// `PhysicalCreateType`: a sink for the values of the query, then the source creating the
/// type once.
#[derive(Debug)]
pub struct PhysicalCreateType {
    types: Vec<LogicalType>,
    children: Vec<PhysicalRef>,
    catalog: Arc<Catalog>,
    info: CreateInfo,
    transaction: Arc<Transaction>,
}

impl PhysicalCreateType {
    pub fn new(
        catalog: Arc<Catalog>,
        info: CreateInfo,
        child: PhysicalRef,
        transaction: Arc<Transaction>,
    ) -> Self {
        Self {
            types: vec![LogicalType::BIGINT],
            children: vec![child],
            catalog,
            info,
            transaction,
        }
    }
}

impl PhysicalOperator for PhysicalCreateType {
    fn name(&self) -> &'static str {
        "CREATE_TYPE"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
}

impl Sink for PhysicalCreateType {
    type Global = CreateTypeGlobal;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<CreateTypeGlobal> {
        Ok(CreateTypeGlobal::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &CreateTypeGlobal) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &CreateTypeGlobal,
        (): &mut (),
    ) -> Result<SinkResult> {
        let mut guard = global.values.lock().unwrap_or_else(PoisonError::into_inner);
        let (values, seen) = &mut *guard;
        let total = values.len() + chunk.len();
        if u32::try_from(total).is_err() {
            return Err(Error::invalid_input(format!(
                "Attempted to create ENUM of size {total}, which exceeds the maximum size of {}",
                u32::MAX
            )));
        }
        let column = chunk.column(0);
        for row in 0..chunk.len() {
            let value = column.get_value(row);
            let Some(text) = value.as_str() else {
                continue;
            };
            if seen.insert(text.to_owned()) {
                values.push(text.to_owned());
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &CreateTypeGlobal, (): ()) -> Result<()> {
        Ok(())
    }
}

impl Source for PhysicalCreateType {
    /// Whether the type was created already.
    type Global = (AtomicBool, Option<SinkState>);
    type Local = ();

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<(AtomicBool, Option<SinkState>)> {
        Ok((AtomicBool::new(false), sink_state.cloned()))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &(AtomicBool, Option<SinkState>)) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (created, sink_state): &(AtomicBool, Option<SinkState>),
        (): &mut (),
    ) -> Result<SourceResult> {
        chunk.set_len(0);
        if created.swap(true, Ordering::AcqRel) {
            return Ok(SourceResult::Finished);
        }
        let values = sink_state
            .as_ref()
            .and_then(|state| state.downcast_ref::<CreateTypeGlobal>())
            .map(|global| {
                global
                    .values
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
                    .clone()
            })
            .ok_or_else(|| Error::internal("CREATE TYPE without its values"))?;
        let mut info = self.info.clone();
        let CreateInfoKind::Type(create_type) = &mut info.kind else {
            return Err(Error::internal("expected a type"));
        };
        create_type.logical_type = LogicalType::enum_type(values);
        CreateEntryAction {
            catalog: Arc::clone(&self.catalog),
            info,
        }
        .execute(&self.transaction)?;
        Ok(SourceResult::Finished)
    }
}
