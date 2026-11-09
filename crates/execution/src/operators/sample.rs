use std::sync::{Mutex, PoisonError};

use roastedduck_ast::{SampleMethod, SampleOptions};
use roastedduck_common::tree_renderer::RenderParams;
use roastedduck_common::{Error, Result};
use roastedduck_function::random::RandomEngine;
use roastedduck_types::{DataChunk, LogicalType, SelectionVector};

use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::sample::{ReservoirSample, ReservoirSamplePercentage};
use crate::{
    ExecutionContext, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source, SourceResult,
};

/// The seed of a sample, which the physical planner always sets.
fn seed(options: &SampleOptions) -> Result<u64> {
    options
        .seed
        .ok_or_else(|| Error::internal("sample without a seed"))
}

/// `USING SAMPLE ... (reservoir)` (DuckDB's `PhysicalReservoirSample`): every input row
/// goes through one reservoir sample, which is emitted at the end.
#[derive(Debug)]
pub struct PhysicalReservoirSample {
    types: Vec<LogicalType>,
    options: SampleOptions,
    children: [PhysicalRef; 1],
}

impl PhysicalReservoirSample {
    pub fn new(types: Vec<LogicalType>, options: SampleOptions, child: PhysicalRef) -> Self {
        Self {
            types,
            options,
            children: [child],
        }
    }

    /// A fresh sample, or `None` for a sample of zero rows (`SampleGlobalSinkState`).
    fn new_sample(&self) -> Result<Option<BlockingSample>> {
        let seed = seed(&self.options)?;
        if self.options.is_percentage {
            let percentage = self.options.sample_size.as_f64().unwrap_or_default();
            if percentage == 0.0 {
                return Ok(None);
            }
            return Ok(Some(BlockingSample::Percentage(ReservoirSamplePercentage::new(
                percentage, seed,
            ))));
        }
        let size = self
            .options
            .sample_size
            .as_i128()
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or_default();
        if size == 0 {
            return Ok(None);
        }
        Ok(Some(BlockingSample::Rows(ReservoirSample::new(size, seed))))
    }
}

/// A row-count or a percentage reservoir sample (DuckDB's `BlockingSample`).
#[derive(Debug)]
pub enum BlockingSample {
    Rows(ReservoirSample),
    Percentage(ReservoirSamplePercentage),
}

impl BlockingSample {
    fn add_to_reservoir(&mut self, chunk: &DataChunk) -> Result<()> {
        match self {
            Self::Rows(sample) => sample.add_to_reservoir(chunk),
            Self::Percentage(sample) => sample.add_to_reservoir(chunk),
        }
    }

    fn get_chunk(&mut self) -> Result<Option<DataChunk>> {
        match self {
            Self::Rows(sample) => Ok(sample.get_chunk()),
            Self::Percentage(sample) => sample.get_chunk(),
        }
    }
}

impl PhysicalOperator for PhysicalReservoirSample {
    fn name(&self) -> &'static str {
        "RESERVOIR_SAMPLE"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        let size = self.options.sample_size.to_varchar().unwrap_or_default();
        let unit = if self.options.is_percentage { "%" } else { " rows" };
        result.insert("Sample Size".to_owned(), format!("{size}{unit}"));
        result
    }
}

impl Sink for PhysicalReservoirSample {
    type Global = Mutex<Option<BlockingSample>>;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::new(self.new_sample()?))
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        (): &mut (),
    ) -> Result<SinkResult> {
        let mut sample = global.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(sample) = sample.as_mut() else {
            return Ok(SinkResult::Finished);
        };
        sample.add_to_reservoir(chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &Self::Global, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, _: &mut Self::Global) -> Result<SinkFinalize> {
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalReservoirSample {
    type Global = SinkState;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<SinkState> {
        sink.cloned()
            .ok_or_else(|| Error::internal("reservoir sample source without its sink state"))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        sink: &SinkState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let sample = sink
            .downcast_ref::<Mutex<Option<BlockingSample>>>()
            .ok_or_else(|| Error::internal("reservoir sample sink state has the wrong type"))?;
        let mut sample = sample.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(sample) = sample.as_mut() else {
            return Ok(SourceResult::Finished);
        };
        match sample.get_chunk()? {
            Some(result) => {
                chunk.set_columns(result.columns().to_vec(), result.len());
                Ok(SourceResult::HaveMoreOutput)
            }
            None => Ok(SourceResult::Finished),
        }
    }
}

/// `USING SAMPLE p% (system | bernoulli)` (DuckDB's `PhysicalStreamingSample`): SYSTEM
/// keeps whole chunks, BERNOULLI single rows, each with probability `p`.
#[derive(Debug)]
pub struct PhysicalStreamingSample {
    types: Vec<LogicalType>,
    method: SampleMethod,
    percentage: f64,
    seed: u64,
    children: [PhysicalRef; 1],
}

impl PhysicalStreamingSample {
    pub fn new(types: Vec<LogicalType>, options: &SampleOptions, child: PhysicalRef) -> Result<Self> {
        Ok(Self {
            types,
            method: options.method,
            percentage: options.sample_size.as_f64().unwrap_or_default() / 100.0,
            seed: seed(options)?,
            children: [child],
        })
    }
}

impl PhysicalOperator for PhysicalStreamingSample {
    fn name(&self) -> &'static str {
        "STREAMING_SAMPLE"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
    }

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert(
            "Sample Method".to_owned(),
            format!("{}: {:.6}%", self.method.name(), 100.0 * self.percentage),
        );
        result
    }
}

impl Operator for PhysicalStreamingSample {
    type Global = ();
    /// The thread's random engine (`StreamingSampleOperatorState`).
    type Local = RandomEngine;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<RandomEngine> {
        Ok(RandomEngine::new(self.seed))
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        random: &mut RandomEngine,
    ) -> Result<OperatorResult> {
        match self.method {
            SampleMethod::SystemSample => {
                // One dice per chunk.
                if random.next_random() <= self.percentage {
                    output.set_columns(input.columns().to_vec(), input.len());
                }
            }
            SampleMethod::BernoulliSample => {
                // One dice per row.
                let selected: Vec<u32> = (0..input.len() as u32)
                    .filter(|_| random.next_random() <= self.percentage)
                    .collect();
                if !selected.is_empty() {
                    let count = selected.len();
                    let sel = SelectionVector::new(selected);
                    output.set_columns(
                        input.columns().iter().map(|column| column.slice(&sel)).collect(),
                        count,
                    );
                }
            }
            _ => return Err(Error::internal("Unsupported sample method for streaming sample")),
        }
        Ok(OperatorResult::NeedMoreInput)
    }
}
