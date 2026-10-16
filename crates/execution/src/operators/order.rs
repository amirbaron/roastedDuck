use std::sync::{Arc, Mutex, PoisonError};

use crate::operator::order_names;
use rustdb_ast::{OrderByNullType, OrderType};
use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_planner::expression::BoundOrderByNode;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_row::sort::{
    MergerLocalState, NullOrder, OrderModifiers, OrderType as SortOrder, Sort, SortColumn, SortConfig,
    SortGlobalSinkState, SortLocalSinkState, SortSource,
};
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// `ORDER BY` (DuckDB's `PhysicalOrder`): sinks its input into sorted runs, then emits
/// the merged result.
#[derive(Debug)]
pub struct PhysicalOrder {
    types: Vec<LogicalType>,
    input_types: Vec<LogicalType>,
    orders: Vec<BoundOrderByNode>,
    /// Input columns to output, in order.
    projections: Vec<usize>,
    children: [PhysicalRef; 1],
}

impl PhysicalOrder {
    /// `projections` selects the output columns from the input (all columns when empty).
    pub fn new(
        types: Vec<LogicalType>,
        orders: Vec<BoundOrderByNode>,
        projections: Vec<usize>,
        child: PhysicalRef,
    ) -> Self {
        let input_types = child.types().to_vec();
        let projections = if projections.is_empty() {
            (0..input_types.len()).collect()
        } else {
            projections
        };
        Self {
            types,
            input_types,
            orders,
            projections,
            children: [child],
        }
    }

    /// Sort keys that are plain input columns are sorted in place; the others are computed
    /// into extra columns appended after the input.
    fn key_columns(&self) -> (Vec<usize>, Vec<Expression>) {
        let mut columns = Vec::with_capacity(self.orders.len());
        let mut computed = Vec::new();
        for order in &self.orders {
            if let ExpressionKind::Reference(reference) = &order.expression.kind {
                columns.push(reference.index as usize);
            } else {
                columns.push(self.input_types.len() + computed.len());
                computed.push(order.expression.clone());
            }
        }
        (columns, computed)
    }

    fn create_sort(&self, ctx: &ExecutionContext) -> Result<Sort> {
        let (columns, computed) = self.key_columns();
        let mut sink_types = self.input_types.clone();
        sink_types.extend(computed.iter().map(|expression| expression.return_type.clone()));
        let orders = self
            .orders
            .iter()
            .zip(columns)
            .map(|(order, column)| {
                Ok(SortColumn {
                    column,
                    modifiers: modifiers(order)?,
                    stats: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Sort::new(
            ctx.env.buffer_manager.clone(),
            orders,
            &sink_types,
            Some(self.projections.clone()),
        )
    }
}

fn modifiers(order: &BoundOrderByNode) -> Result<OrderModifiers> {
    let order_type = match order.order_type {
        OrderType::Ascending => SortOrder::Ascending,
        OrderType::Descending => SortOrder::Descending,
        other => return Err(Error::internal(format!("unresolved order type {other:?}"))),
    };
    let nulls = match order.null_order {
        OrderByNullType::NullsFirst => NullOrder::NullsFirst,
        OrderByNullType::NullsLast => NullOrder::NullsLast,
        other => return Err(Error::internal(format!("unresolved null order {other:?}"))),
    };
    Ok(OrderModifiers::new(order_type, nulls))
}

impl PhysicalOperator for PhysicalOrder {
    fn name(&self) -> &'static str {
        "ORDER_BY"
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
        result.insert("__order_by__".to_owned(), order_names(&self.orders));
        result
    }
}

/// The sort, its sink state while sinking, and its merged source afterwards.
#[derive(Debug)]
pub struct OrderGlobalState {
    sort: Sort,
    sink: Option<SortGlobalSinkState>,
    source: Option<SortSource>,
}

/// Per-thread sink state: the thread's sorted run and executors for computed keys.
#[derive(Debug)]
pub struct OrderLocalState {
    sort: SortLocalSinkState,
    keys: Option<ExpressionExecutor>,
}

impl Sink for PhysicalOrder {
    type Global = OrderGlobalState;
    type Local = OrderLocalState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<OrderGlobalState> {
        let sort = self.create_sort(ctx)?;
        let config = SortConfig {
            threads: ctx.env.threads,
            connections: 1,
            ..SortConfig::default()
        };
        let sink = sort.global_sink_state(config);
        Ok(OrderGlobalState {
            sort,
            sink: Some(sink),
            source: None,
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, global: &OrderGlobalState) -> Result<OrderLocalState> {
        let (_, computed) = self.key_columns();
        let keys = (!computed.is_empty()).then(|| ExpressionExecutor::new(computed));
        Ok(OrderLocalState {
            sort: global.sort.local_sink_state(),
            keys,
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &OrderGlobalState,
        local: &mut OrderLocalState,
    ) -> Result<SinkResult> {
        let sink = global
            .sink
            .as_ref()
            .ok_or_else(|| Error::internal("ORDER BY sink after finalize"))?;
        match &mut local.keys {
            None => global.sort.sink(chunk, sink, &mut local.sort)?,
            Some(executor) => {
                let mut keys = DataChunk::default();
                executor.execute(chunk, &mut keys)?;
                let mut columns = chunk.columns().to_vec();
                columns.extend(keys.into_columns());
                global
                    .sort
                    .sink(&DataChunk::new(columns, chunk.len()), sink, &mut local.sort)?;
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &OrderGlobalState, local: OrderLocalState) -> Result<()> {
        let sink = global
            .sink
            .as_ref()
            .ok_or_else(|| Error::internal("ORDER BY combine after finalize"))?;
        global.sort.combine(sink, local.sort)
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut OrderGlobalState) -> Result<SinkFinalize> {
        let sink = global
            .sink
            .take()
            .ok_or_else(|| Error::internal("ORDER BY finalized twice"))?;
        global.source = global.sort.finalize(sink)?;
        Ok(if global.source.is_some() {
            SinkFinalize::Ready
        } else {
            SinkFinalize::NoOutputPossible
        })
    }
}

/// Source state: the finalized sink state and a reader per thread.
#[derive(Debug)]
pub struct OrderSourceState {
    sink: SinkState,
}

impl Source for PhysicalOrder {
    type Global = OrderSourceState;
    type Local = Mutex<Option<MergerLocalState>>;

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<OrderSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("ORDER BY source requires its sink state"))?;
        Ok(OrderSourceState {
            sink: Arc::clone(sink),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &OrderSourceState) -> Result<Self::Local> {
        Ok(Mutex::new(None))
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &OrderSourceState,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        let state = global
            .sink
            .downcast_ref::<OrderGlobalState>()
            .ok_or_else(|| Error::internal("ORDER BY sink state has the wrong type"))?;
        let Some(source) = &state.source else {
            return Ok(SourceResult::Finished);
        };
        let local = local.get_mut().unwrap_or_else(PoisonError::into_inner);
        let reader = local.get_or_insert_with(|| source.local_state());
        Ok(if source.get_data(chunk, reader)? {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
