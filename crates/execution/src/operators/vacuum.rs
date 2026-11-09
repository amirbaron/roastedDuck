//! `VACUUM` / `ANALYZE` of a table (DuckDB's `PhysicalVacuum`): every row of the analyzed
//! columns goes into a fresh distinct-count sketch, which then replaces the column's.

use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::Result;
use roastedduck_storage::DataTable;
use roastedduck_storage::statistics::DistinctStatistics;
use roastedduck_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// One sketch per analyzed column; `None` for types without distinct statistics
/// (`VacuumGlobalSinkState` / `VacuumLocalSinkState`).
#[derive(Debug)]
pub struct VacuumStatistics(Vec<Option<DistinctStatistics>>);

impl VacuumStatistics {
    fn new(types: &[LogicalType]) -> Self {
        Self(
            types
                .iter()
                .map(|ty| DistinctStatistics::type_is_supported(ty).then(DistinctStatistics::default))
                .collect(),
        )
    }

    fn merge(&mut self, other: &Self) {
        for (target, source) in self.0.iter_mut().zip(&other.0) {
            if let (Some(target), Some(source)) = (target, source) {
                target.merge(source);
            }
        }
    }
}

/// `PhysicalVacuum`: sinks the analyzed columns, then sets their distinct statistics.
#[derive(Debug)]
pub struct PhysicalVacuum {
    types: Vec<LogicalType>,
    children: Vec<PhysicalRef>,
    storage: Arc<DataTable>,
    /// The physical index of each analyzed column.
    column_id_map: Vec<usize>,
}

impl PhysicalVacuum {
    pub fn new(child: PhysicalRef, storage: Arc<DataTable>, column_id_map: Vec<usize>) -> Self {
        Self {
            types: vec![LogicalType::BOOLEAN],
            children: vec![child],
            storage,
            column_id_map,
        }
    }

    fn column_types(&self) -> &[LogicalType] {
        self.children[0].types()
    }
}

impl PhysicalOperator for PhysicalVacuum {
    fn name(&self) -> &'static str {
        "VACUUM"
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

impl Sink for PhysicalVacuum {
    type Global = Mutex<VacuumStatistics>;
    type Local = VacuumStatistics;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::new(VacuumStatistics::new(self.column_types())))
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<VacuumStatistics> {
        Ok(VacuumStatistics::new(self.column_types()))
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut VacuumStatistics,
    ) -> Result<SinkResult> {
        for (index, statistics) in local.0.iter_mut().enumerate() {
            if let Some(statistics) = statistics {
                statistics.update(chunk.column(index), chunk.len())?;
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, local: VacuumStatistics) -> Result<()> {
        global
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .merge(&local);
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let statistics = std::mem::take(&mut global.get_mut().unwrap_or_else(PoisonError::into_inner).0);
        for (&column, statistics) in self.column_id_map.iter().zip(statistics) {
            self.storage.set_distinct(column, statistics);
        }
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalVacuum {
    type Global = ();
    type Local = ();

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        _: Option<&crate::operator::SinkState>,
    ) -> Result<()> {
        Ok(())
    }

    fn local_source_state(&self, _: &ExecutionContext, (): &()) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (): &(),
        (): &mut (),
    ) -> Result<SourceResult> {
        chunk.set_len(0);
        Ok(SourceResult::Finished)
    }
}
