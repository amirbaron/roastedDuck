//! `arg_min`/`arg_max` (`min_by`/`max_by`, `argmin`/`argmax`), their `_null` and
//! `_nulls_last` variants and the top-N forms (DuckDB's `arg_min_max.cpp`).
//!
//! Overloads mirror DuckDB's: typed `(arg, by)` pairs over a fixed list of types, DECIMAL
//! arguments, `(ANY, by)` (the argument kept as a sort key) and `(ANY, ANY)` (both as sort
//! keys). Overload resolution therefore picks the same overloads and result types.

use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_function::aggregate::{Aggregate, AggregateInput, AggregateOps, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, Vector};

use crate::aggregate::distributive::minmax::check_top_n;
use crate::aggregate::heap::TopN;
use crate::aggregate::nested::list::append_list;
use crate::aggregate::stored::{Bytes, Fixed, SortKey, StoredKind};
use crate::signature::type_id;

/// Which NULLs take part (DuckDB's `ArgMinMaxNullHandling`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names, reason = "DuckDB's variant names")]
pub(crate) enum NullHandling {
    /// `arg_min`: rows with a NULL argument or key are ignored.
    IgnoreAnyNull,
    /// `arg_min_null`: NULL arguments can be the result; NULL keys are ignored.
    HandleArgNull,
    /// `arg_min_nulls_last`: NULL keys sort last.
    HandleAnyNull,
}

/// Bind data of every `arg_min/arg_max` overload (DuckDB's `ArgMinMaxFunctionData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArgMinMaxData {
    null_handling: NullHandling,
}

impl FunctionData for ArgMinMaxData {
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The state: whether initialised, the argument (`None` = NULL) and the key (`None` = NULL).
struct ArgState<A, B> {
    initialized: bool,
    arg: Option<A>,
    value: Option<B>,
}

impl<A: Clone, B: Clone> Clone for ArgState<A, B> {
    fn clone(&self) -> Self {
        Self {
            initialized: self.initialized,
            arg: self.arg.clone(),
            value: self.value.clone(),
        }
    }
}

/// `arg_min`/`arg_max` storing arguments as `A` and keys as `B`; `GREATER` picks the greatest
/// key (otherwise the smallest).
struct ArgMinMax<A, B, const GREATER: bool>(PhantomData<fn() -> (A, B)>);

impl<A: StoredKind, B: StoredKind, const GREATER: bool> ArgMinMax<A, B, GREATER> {
    #[inline]
    fn better(new: B::Ref<'_>, current: B::Ref<'_>) -> bool {
        let ordering = B::cmp(new, current);
        if GREATER {
            ordering.is_gt()
        } else {
            ordering.is_lt()
        }
    }
}

impl<A: StoredKind, B: StoredKind, const GREATER: bool> Aggregate for ArgMinMax<A, B, GREATER> {
    type State = ArgState<A::Owned, B::Owned>;

    fn initialize(&self) -> Self::State {
        ArgState {
            initialized: false,
            arg: None,
            value: None,
        }
    }

    fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let null_handling = input.bind_data::<ArgMinMaxData>()?.null_handling;
        let args = A::prepare(&inputs[0], count)?;
        let keys = B::prepare(&inputs[1], count)?;
        for row in 0..count {
            let arg = A::get(&args, row);
            if null_handling == NullHandling::IgnoreAnyNull && arg.is_none() {
                continue;
            }
            let Some(key) = B::get(&keys, row) else {
                if null_handling == NullHandling::HandleAnyNull {
                    let state = states.get(row);
                    if !state.initialized
                        && let Some(arg) = arg
                    {
                        state.initialized = true;
                        state.value = None;
                        state.arg = Some(A::to_owned(arg));
                    }
                }
                continue;
            };
            let state = states.get(row);
            let replace = match &state.value {
                Some(current) if state.initialized => Self::better(key, B::borrow(current)),
                _ => true,
            };
            if replace {
                match &mut state.value {
                    Some(current) => B::assign(current, key),
                    None => state.value = Some(B::to_owned(key)),
                }
                match (arg, &mut state.arg) {
                    (Some(arg), Some(current)) => A::assign(current, arg),
                    (arg, slot) => *slot = arg.map(A::to_owned),
                }
                state.initialized = true;
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        if !source.initialized {
            return Ok(());
        }
        let replace = !target.initialized
            || match (&target.value, &source.value) {
                (None, _) => true,
                (Some(target), Some(source)) => Self::better(B::borrow(source), B::borrow(target)),
                (Some(_), None) => false,
            };
        if replace {
            *target = source.clone();
        }
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        match &state.arg {
            Some(arg) if state.initialized => A::write(arg, result, row),
            _ => {
                result.set_null(row);
                Ok(())
            }
        }
    }
}

/// Builds the operations for an argument kind and a key physical type.
fn ops_for_key<A: StoredKind, const MAX: bool>(key: &LogicalType) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match key.physical_type() {
        P::Int32 => Arc::new(Typed(ArgMinMax::<A, Fixed<i32>, MAX>(PhantomData))),
        P::Int64 => Arc::new(Typed(ArgMinMax::<A, Fixed<i64>, MAX>(PhantomData))),
        P::Int128 => Arc::new(Typed(ArgMinMax::<A, Fixed<i128>, MAX>(PhantomData))),
        P::Double => Arc::new(Typed(ArgMinMax::<A, Fixed<f64>, MAX>(PhantomData))),
        P::Varchar => Arc::new(Typed(ArgMinMax::<A, Bytes, MAX>(PhantomData))),
        // Generic keys are sort keys in the aggregate's direction: the smallest key wins.
        _ => Arc::new(Typed(ArgMinMax::<A, SortKey<MAX>, false>(PhantomData))),
    }
}

/// The operations for argument type `arg` (by physical type; `None` for the `ANY`
/// argument stored as a sort key) and key type `key`.
fn arg_min_max_ops<const MAX: bool>(arg: Option<&LogicalType>, key: &LogicalType) -> Arc<dyn AggregateOps> {
    use PhysicalType as P;
    match arg.map(LogicalType::physical_type) {
        Some(P::Int16) => ops_for_key::<Fixed<i16>, MAX>(key),
        Some(P::Int32) => ops_for_key::<Fixed<i32>, MAX>(key),
        Some(P::Int64) => ops_for_key::<Fixed<i64>, MAX>(key),
        Some(P::Int128) => ops_for_key::<Fixed<i128>, MAX>(key),
        Some(P::Double) => ops_for_key::<Fixed<f64>, MAX>(key),
        Some(P::Varchar) => ops_for_key::<Bytes, MAX>(key),
        _ => ops_for_key::<SortKey<false>, MAX>(key),
    }
}

/// The key types of the typed overloads (DuckDB's `ArgMaxByTypes`).
fn by_types() -> [LogicalType; 9] {
    [
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::DOUBLE,
        LogicalType::VARCHAR,
        LogicalType::DATE,
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_TZ,
        LogicalType::BLOB,
    ]
}

/// The bind of the typed and `ANY` overloads: the result has the argument's exact type.
fn bind_typed(
    null_handling: NullHandling,
) -> impl Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync + 'static
{
    move |function, input| {
        let ty = input.argument_types[0].clone();
        function.arguments[0] = ty.clone();
        function.return_type = ty;
        Ok(Some(Arc::new(ArgMinMaxData { null_handling })))
    }
}

/// DuckDB's `BindDecimalArgMinMax`.
fn bind_decimal<const MAX: bool>(
    null_handling: NullHandling,
) -> impl Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync + 'static
{
    move |function, input| {
        let decimal_type = input.argument_types[0].clone();
        let mut by_type = input.argument_types[1].clone();
        if !by_types()
            .iter()
            .any(|ty| ty.physical_type() == by_type.physical_type())
        {
            // DuckDB keeps the last implicitly castable key type.
            if let Some(ty) = by_types()
                .into_iter()
                .rfind(|ty| rustdb_types::implicit_cast_cost(&by_type, ty) >= 0)
            {
                by_type = ty;
            }
        }
        function.ops = arg_min_max_ops::<MAX>(Some(&decimal_type), &by_type);
        function.clear_catalog_entry();
        function.arguments = vec![decimal_type.clone(), by_type];
        function.return_type = decimal_type;
        Ok(Some(Arc::new(ArgMinMaxData { null_handling })))
    }
}

/// DuckDB's `AddArgMinMaxFunctions`.
fn add_arg_min_max<const MAX: bool>(set: &mut AggregateFunctionSet, null_handling: NullHandling) {
    let first = set.functions.len();
    add_arg_min_max_overloads::<MAX>(set, null_handling);
    // `ArgMinMaxBase::Bind` / `VectorArgMinMaxBase::Bind`: a VARCHAR ordering argument is
    // compared collated.
    for function in &mut set.functions[first..] {
        function.collation = rustdb_function::AggregateCollation::Argument(1);
    }
}

fn add_arg_min_max_overloads<const MAX: bool>(set: &mut AggregateFunctionSet, null_handling: NullHandling) {
    let arg_types = [
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::DOUBLE,
        LogicalType::VARCHAR,
        LogicalType::DATE,
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_TZ,
        LogicalType::BLOB,
    ];
    for arg in &arg_types {
        for by in by_types() {
            set.add(
                AggregateFunction::new(
                    "",
                    vec![arg.clone(), by.clone()],
                    arg.clone(),
                    rustdb_function::aggregate::Unbound,
                )
                .with_bind(bind_typed(null_handling)),
            );
            let last = set.functions.len() - 1;
            set.functions[last].ops = arg_min_max_ops::<MAX>(Some(arg), &by);
        }
    }
    for by in by_types() {
        set.add(AggregateFunction::unbound(
            vec![type_id(LogicalTypeId::Decimal), by],
            type_id(LogicalTypeId::Decimal),
            bind_decimal::<MAX>(null_handling),
        ));
    }
    for by in by_types() {
        let mut function = AggregateFunction::new(
            "",
            vec![LogicalType::ANY, by.clone()],
            LogicalType::ANY,
            rustdb_function::aggregate::Unbound,
        )
        .with_bind(bind_typed(null_handling));
        function.ops = arg_min_max_ops::<MAX>(None, &by);
        set.add(function);
    }
    let mut generic = AggregateFunction::new(
        "",
        vec![LogicalType::ANY, LogicalType::ANY],
        LogicalType::ANY,
        rustdb_function::aggregate::Unbound,
    )
    .with_bind(move |function, input| {
        let ty = input.argument_types[0].clone();
        function.arguments = vec![ty.clone(), input.argument_types[1].clone()];
        function.return_type = ty;
        Ok(Some(Arc::new(ArgMinMaxData { null_handling })))
    });
    generic.ops = arg_min_max_ops::<MAX>(None, &LogicalType::ANY);
    set.add(generic);
}

/// A `(key, argument)` pair of the top-N form (`None` = NULL).
type KeyArg<K, A> = (Option<K>, Option<A>);

/// The state of the top-N form: the best `(key, argument)` pairs.
struct ArgTopNState<A, B> {
    heap: Option<TopN<KeyArg<B, A>>>,
}

impl<A, B> Default for ArgTopNState<A, B> {
    fn default() -> Self {
        Self { heap: None }
    }
}

/// `arg_min(x, by, n)` (DuckDB's `ArgMinMaxNState`) storing arguments as `A` and keys as `B`
/// (DuckDB's `MinMaxFixedValue` / `MinMaxStringValue` / `MinMaxFallbackValue`); NULL keys
/// (in the `_nulls_last` form) order last.
struct ArgMinMaxN<A, B, const MAX: bool> {
    null_handling: NullHandling,
    _kinds: PhantomData<fn() -> (A, B)>,
}

impl<A: StoredKind, B: StoredKind, const MAX: bool> ArgMinMaxN<A, B, MAX> {
    /// Whether key `a` is better than key `b` (NULL keys are worst).
    #[inline]
    fn key_better(a: Option<B::Ref<'_>>, b: Option<B::Ref<'_>>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => {
                let ordering = B::cmp(a, b);
                if MAX { ordering.is_gt() } else { ordering.is_lt() }
            }
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// The heap order: entries with better keys first.
    fn less(a: &KeyArg<B::Owned, A::Owned>, b: &KeyArg<B::Owned, A::Owned>) -> bool {
        Self::key_better(a.0.as_ref().map(B::borrow), b.0.as_ref().map(B::borrow))
    }

    #[inline]
    fn insert(heap: &mut TopN<KeyArg<B::Owned, A::Owned>>, key: Option<B::Ref<'_>>, arg: Option<A::Ref<'_>>) {
        heap.insert_with(
            &Self::less,
            |top| Self::key_better(key, top.0.as_ref().map(B::borrow)),
            || (key.map(B::to_owned), arg.map(A::to_owned)),
            |slot| {
                match (key, &mut slot.0) {
                    (Some(key), Some(current)) => B::assign(current, key),
                    (key, current) => *current = key.map(B::to_owned),
                }
                match (arg, &mut slot.1) {
                    (Some(arg), Some(current)) => A::assign(current, arg),
                    (arg, current) => *current = arg.map(A::to_owned),
                }
            },
        );
    }
}

impl<A: StoredKind, B: StoredKind, const MAX: bool> Aggregate for ArgMinMaxN<A, B, MAX> {
    type State = ArgTopNState<A::Owned, B::Owned>;

    fn initialize(&self) -> Self::State {
        ArgTopNState::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let args = A::prepare(&inputs[0], count)?;
        let keys = B::prepare(&inputs[1], count)?;
        let n = inputs[2].unified::<i64>();
        for row in 0..count {
            let arg = A::get(&args, row);
            let key = B::get(&keys, row);
            if self.null_handling == NullHandling::IgnoreAnyNull && (arg.is_none() || key.is_none()) {
                continue;
            }
            let state = states.get(row);
            let heap = match &mut state.heap {
                Some(heap) => heap,
                None => state
                    .heap
                    .insert(TopN::new(check_top_n(n.get(row), "arg_min/arg_max")?)),
            };
            Self::insert(heap, key, arg);
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        crate::aggregate::distributive::minmax::combine_top_n(
            source.heap.as_ref(),
            &mut target.heap,
            |heap, (key, arg)| Self::insert(heap, key.as_ref().map(B::borrow), arg.as_ref().map(A::borrow)),
        )
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        let Some(heap) = state.heap.as_mut().filter(|heap| !heap.is_empty()) else {
            result.set_null(row);
            return Ok(());
        };
        let sorted = heap.sorted(&Self::less);
        append_list(
            result,
            row,
            |child, offset| {
                for (index, (_, arg)) in sorted.iter().enumerate() {
                    match arg {
                        Some(arg) => A::write(arg, child, offset + index)?,
                        None => child.set_null(offset + index),
                    }
                }
                Ok(())
            },
            sorted.len(),
        )
    }
}

/// Dispatches `$body` with `$K` bound to how the top-N `arg_min`/`arg_max` stores values of
/// `$ty`: by value for the types DuckDB specialises (`INT32`, `INT64`, `FLOAT`, `DOUBLE`), as
/// bytes for VARCHAR, as ascending sort keys otherwise (which order like the values).
macro_rules! with_top_n_kind {
    ($ty:expr, $K:ident => $body:expr) => {{
        use rustdb_types::PhysicalType as P;
        match $ty.physical_type() {
            P::Int32 => {
                type $K = Fixed<i32>;
                $body
            }
            P::Int64 => {
                type $K = Fixed<i64>;
                $body
            }
            P::Float => {
                type $K = Fixed<f32>;
                $body
            }
            P::Double => {
                type $K = Fixed<f64>;
                $body
            }
            P::Varchar => {
                type $K = Bytes;
                $body
            }
            _ => {
                type $K = SortKey<false>;
                $body
            }
        }
    }};
}

/// DuckDB's `AddArgMinMaxNFunction`.
fn arg_min_max_n<const MAX: bool>(null_handling: NullHandling) -> AggregateFunction {
    AggregateFunction::unbound(
        vec![LogicalType::ANY, LogicalType::ANY, LogicalType::BIGINT],
        LogicalType::list(LogicalType::ANY),
        move |function, input| {
            if input
                .argument_types
                .iter()
                .any(|ty| ty.id() == LogicalTypeId::Unknown)
            {
                return Err(crate::parameter_not_resolved());
            }
            let ty = input.argument_types[0].clone();
            let by = input.argument_types[1].clone();
            function.ops = with_top_n_kind!(ty, A => with_top_n_kind!(by, B => {
                let ops: Arc<dyn AggregateOps> = Arc::new(Typed(ArgMinMaxN::<A, B, MAX> {
                    null_handling,
                    _kinds: PhantomData,
                }));
                ops
            }));
            function.arguments = vec![ty.clone(), by, LogicalType::BIGINT];
            function.return_type = LogicalType::list(ty);
            Ok(Some(Arc::new(ArgMinMaxData { null_handling })))
        },
    )
}

fn arg_set<const MAX: bool>(name: &str, null_handling: NullHandling, top_n: bool) -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new(name);
    add_arg_min_max::<MAX>(&mut set, null_handling);
    if top_n {
        set.add(arg_min_max_n::<MAX>(null_handling));
    }
    set.renamed(name)
}

/// The `arg_min` set (aliases `argmin`, `min_by`).
pub fn arg_min_functions() -> AggregateFunctionSet {
    arg_set::<false>("arg_min", NullHandling::IgnoreAnyNull, true)
}

/// The `arg_max` set (aliases `argmax`, `max_by`).
pub fn arg_max_functions() -> AggregateFunctionSet {
    arg_set::<true>("arg_max", NullHandling::IgnoreAnyNull, true)
}

/// The `arg_min_null` set.
pub fn arg_min_null_functions() -> AggregateFunctionSet {
    arg_set::<false>("arg_min_null", NullHandling::HandleArgNull, false)
}

/// The `arg_max_null` set.
pub fn arg_max_null_functions() -> AggregateFunctionSet {
    arg_set::<true>("arg_max_null", NullHandling::HandleArgNull, false)
}

/// The `arg_min_nulls_last` set.
pub fn arg_min_nulls_last_functions() -> AggregateFunctionSet {
    arg_set::<false>("arg_min_nulls_last", NullHandling::HandleAnyNull, true)
}

/// The `arg_max_nulls_last` set.
pub fn arg_max_nulls_last_functions() -> AggregateFunctionSet {
    arg_set::<true>("arg_max_nulls_last", NullHandling::HandleAnyNull, true)
}
