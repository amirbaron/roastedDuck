//! Internal optimizer hooks for compiled-in extensions (DuckDB's
//! `OptimizerExtension`). This is a workspace implementation detail, not a
//! public plugin or binary-extension interface.

use std::fmt::Debug;
use std::sync::{Arc, PoisonError, RwLock};

use roastedduck_common::{Error, Result};
use roastedduck_function::ObjectCache;
use roastedduck_planner::{ClientContext, LogicalOperator};

use crate::Optimizer;

/// A compiled-in rule's callbacks, run before and after the built-in passes.
///
/// Implementations must preserve the query's result and output bindings. New
/// table bindings come from [`Optimizer::generate_table_index`]. Both callbacks
/// honor the `extension` optimizer setting; their order is registration order.
#[doc(hidden)]
pub trait OptimizerRule: Debug + Send + Sync {
    /// A unique internal name, used to reject accidental duplicate registration.
    fn name(&self) -> &'static str;

    /// Rewrites before the built-in optimizer passes.
    fn pre_optimize(&self, _optimizer: &mut Optimizer, plan: LogicalOperator) -> Result<LogicalOperator> {
        Ok(plan)
    }

    /// Rewrites after the built-in optimizer passes.
    fn post_optimize(&self, _optimizer: &mut Optimizer, plan: LogicalOperator) -> Result<LogicalOperator> {
        Ok(plan)
    }
}

/// A query-owned snapshot: invoking callbacks never holds a registry lock.
#[doc(hidden)]
pub type OptimizerRules = Arc<[Arc<dyn OptimizerRule>]>;

/// The ordered first-party rules belonging to one database instance.
///
/// The embedding database owns this registry and registers its compiled-in
/// extensions during initialization/loading. A query uses [`Self::snapshot`],
/// so a subsequent registration affects later optimizers only.
#[derive(Debug, Default)]
#[doc(hidden)]
pub struct OptimizerRuleRegistry {
    rules: RwLock<Vec<Arc<dyn OptimizerRule>>>,
}

const REGISTRY_CACHE_KEY: &str = "roastedduck.internal.optimizer_rule_registry.v1";

/// The private cache carrier keeps attachment identity separate from rule state.
#[derive(Debug)]
struct AttachedRegistry(Arc<OptimizerRuleRegistry>);

impl OptimizerRuleRegistry {
    /// Attaches this database's registry for every optimizer construction path.
    /// Reattaching the same registry is harmless; replacing it is an error.
    pub fn attach(self: &Arc<Self>, cache: &ObjectCache) -> Result<()> {
        let attached = cache.get_or_insert_with(REGISTRY_CACHE_KEY, || AttachedRegistry(Arc::clone(self)));
        if !Arc::ptr_eq(&attached.0, self) {
            return Err(Error::internal(
                "A different internal optimizer rule registry is already attached to this database",
            ));
        }
        Ok(())
    }

    /// The snapshot for this connection, without introducing a planner dependency
    /// on the optimizer crate. Prepared and JSON-plan paths use the same cache.
    pub(crate) fn for_context(context: &ClientContext) -> OptimizerRules {
        context
            .function_client
            .object_cache()
            .and_then(|cache| cache.get::<AttachedRegistry>(REGISTRY_CACHE_KEY))
            .map_or_else(|| Arc::from([]), |attached| attached.0.snapshot())
    }

    /// Appends a rule, rejecting duplicate names without changing the registry.
    pub fn register(&self, rule: Arc<dyn OptimizerRule>) -> Result<()> {
        let mut rules = self.rules.write().unwrap_or_else(PoisonError::into_inner);
        if rule.name().is_empty() || rules.iter().any(|existing| existing.name() == rule.name()) {
            return Err(Error::internal(format!(
                "Invalid or duplicate internal optimizer rule name: {}",
                rule.name()
            )));
        }
        rules.push(rule);
        Ok(())
    }

    /// Captures the current callbacks in registration order, releasing the lock.
    pub fn snapshot(&self) -> OptimizerRules {
        self.rules
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions inspect required plan structure"
)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, Weak};

    use roastedduck_catalog::{Catalog, Transaction};
    use roastedduck_function::{FunctionClient, cast::CastFunctionSet};
    use roastedduck_planner::{Expression, ExpressionEvaluator, ExpressionKind, LogicalKind, PlannerConfig};
    use roastedduck_types::Value;

    use super::*;
    use crate::{OptimizerConfig, OptimizerType};

    #[derive(Debug)]
    struct ConstantEvaluator;

    impl ExpressionEvaluator for ConstantEvaluator {
        fn evaluate_scalar(&self, expression: &Expression) -> Result<Value> {
            match &expression.kind {
                ExpressionKind::Constant(constant) => Ok(constant.value.clone()),
                _ => Err(Error::internal("test plan evaluated a nonconstant expression")),
            }
        }
    }

    fn context() -> Result<Arc<ClientContext>> {
        Ok(Arc::new(ClientContext {
            catalogs: vec![Arc::new(Catalog::new("memory")?)],
            search_path: Vec::new(),
            raw_search_path: Vec::new(),
            has_default_database: true,
            view_search_path: Vec::new(),
            view_default_schema_precedence: false,
            transaction: Arc::new(Transaction::system()),
            casts: Arc::new(CastFunctionSet::new()),
            evaluator: Arc::new(ConstantEvaluator),
            config: PlannerConfig::default(),
            prepared_statements: roastedduck_common::CaseInsensitiveMap::default(),
            function_client: Arc::new(FunctionClient::default()),
            dependency_recorder: Arc::default(),
            database_state: None,
            user_variables: roastedduck_common::CaseInsensitiveMap::default(),
            replacement_scans: Vec::new(),
        }))
    }

    fn plan() -> LogicalOperator {
        LogicalOperator::projection(
            40,
            vec![Expression::constant(Value::integer(42))],
            LogicalOperator::new(LogicalKind::DummyScan { table_index: 41 }),
        )
    }

    fn has_filter(plan: &LogicalOperator) -> bool {
        matches!(plan.kind, LogicalKind::Filter { .. }) || plan.children.iter().any(has_filter)
    }

    #[derive(Debug, Default)]
    struct Observations {
        calls: Vec<(&'static str, &'static str)>,
        bindings: Vec<u64>,
    }

    #[derive(Debug)]
    struct FilterRule {
        name: &'static str,
        observations: Arc<Mutex<Observations>>,
    }

    impl OptimizerRule for FilterRule {
        fn name(&self) -> &'static str {
            self.name
        }

        fn pre_optimize(&self, optimizer: &mut Optimizer, plan: LogicalOperator) -> Result<LogicalOperator> {
            let mut observations = self.observations.lock().unwrap();
            observations.calls.push((self.name, "pre"));
            observations.bindings.push(optimizer.generate_table_index());
            // A redundant predicate introduced by an internal rewrite must be
            // simplified by the real built-in passes before post rules inspect it.
            Ok(LogicalOperator::filter(
                Expression::constant(Value::boolean(true)),
                plan,
            ))
        }

        fn post_optimize(&self, optimizer: &mut Optimizer, plan: LogicalOperator) -> Result<LogicalOperator> {
            assert!(!has_filter(&plan), "post rule ran before filter simplification");
            let mut observations = self.observations.lock().unwrap();
            observations.calls.push((self.name, "post"));
            observations.bindings.push(optimizer.generate_table_index());
            Ok(plan)
        }
    }

    fn add_rule(
        registry: &OptimizerRuleRegistry,
        name: &'static str,
        observations: &Arc<Mutex<Observations>>,
    ) -> Result<()> {
        registry.register(Arc::new(FilterRule {
            name,
            observations: Arc::clone(observations),
        }))
    }

    #[test]
    fn callbacks_surround_real_passes_preserving_results_and_bindings() -> Result<()> {
        let registry = OptimizerRuleRegistry::default();
        let observations = Arc::new(Mutex::new(Observations::default()));
        add_rule(&registry, "first", &observations)?;
        add_rule(&registry, "second", &observations)?;
        let mut optimizer =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(registry.snapshot());
        optimizer.reserve_table_indexes(100);
        let before = plan();
        let bindings = before.column_bindings();
        let after = optimizer.optimize(before)?;
        assert!(!has_filter(&after));
        assert_eq!(after.column_bindings(), bindings);
        let ExpressionKind::Constant(result) = &after.expressions[0].kind else {
            panic!("constant query output was lost");
        };
        assert_eq!(result.value, Value::integer(42));
        let observations = observations.lock().unwrap();
        assert_eq!(
            observations.calls,
            [
                ("first", "pre"),
                ("second", "pre"),
                ("first", "post"),
                ("second", "post")
            ]
        );
        assert!(observations.bindings.iter().all(|&index| index >= 100));
        assert!(observations.bindings.windows(2).all(|pair| pair[0] < pair[1]));
        let timings = optimizer.timings();
        assert_eq!(
            timings
                .iter()
                .filter(|(kind, _)| *kind == OptimizerType::Extension)
                .count(),
            4
        );
        assert_eq!(timings.first().unwrap().0, OptimizerType::Extension);
        assert_eq!(timings.last().unwrap().0, OptimizerType::Extension);
        assert!(
            timings
                .iter()
                .any(|(kind, _)| *kind == OptimizerType::FilterPushdown)
        );
        Ok(())
    }

    #[test]
    fn disabling_extensions_preserves_builtin_optimization() -> Result<()> {
        let registry = OptimizerRuleRegistry::default();
        let observations = Arc::new(Mutex::new(Observations::default()));
        add_rule(&registry, "disabled", &observations)?;
        let mut config = OptimizerConfig::default();
        config.disabled.insert(OptimizerType::Extension);
        let mut optimizer = Optimizer::new(context()?, config).with_rules(registry.snapshot());
        let filtered = LogicalOperator::filter(Expression::constant(Value::boolean(true)), plan());
        let optimized = optimizer.optimize(filtered)?;
        assert!(!has_filter(&optimized));
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);
        assert!(
            !optimizer
                .timings()
                .iter()
                .any(|(kind, _)| *kind == OptimizerType::Extension)
        );

        let mut optimizer = Optimizer::new(
            context()?,
            OptimizerConfig {
                enabled: false,
                ..Default::default()
            },
        )
        .with_rules(registry.snapshot());
        let filtered = LogicalOperator::filter(Expression::constant(Value::boolean(true)), plan());
        assert!(has_filter(&optimizer.optimize(filtered)?));
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);
        assert_eq!(optimizer.timings(), []);
        Ok(())
    }

    #[test]
    fn snapshots_and_database_registries_remain_isolated() -> Result<()> {
        let registry = OptimizerRuleRegistry::default();
        let independent = OptimizerRuleRegistry::default();
        let observations = Arc::new(Mutex::new(Observations::default()));
        add_rule(&registry, "first", &observations)?;
        let mut earlier =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(registry.snapshot());
        add_rule(&registry, "later", &observations)?;
        assert!(add_rule(&registry, "first", &observations).is_err());
        earlier.optimize(plan())?;
        assert_eq!(
            observations.lock().unwrap().calls,
            [("first", "pre"), ("first", "post")]
        );
        observations.lock().unwrap().calls.clear();
        let mut other =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(independent.snapshot());
        other.optimize(plan())?;
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);
        let mut later =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(registry.snapshot());
        later.optimize(plan())?;
        assert_eq!(
            observations.lock().unwrap().calls,
            [
                ("first", "pre"),
                ("later", "pre"),
                ("first", "post"),
                ("later", "post")
            ]
        );
        Ok(())
    }

    #[test]
    fn constructors_discover_their_database_registry_without_replacement() -> Result<()> {
        let first = Arc::new(OptimizerRuleRegistry::default());
        let second = Arc::new(OptimizerRuleRegistry::default());
        let first_cache = Arc::new(ObjectCache::default());
        let second_cache = Arc::new(ObjectCache::default());
        first.attach(&first_cache)?;
        first.attach(&first_cache)?;
        second.attach(&second_cache)?;
        assert!(second.attach(&first_cache).is_err());

        let observations = Arc::new(Mutex::new(Observations::default()));
        add_rule(&first, "first-database", &observations)?;
        add_rule(&second, "second-database", &observations)?;
        let first_context = context()?;
        first_context.function_client.set_object_cache(first_cache);
        let second_context = context()?;
        second_context.function_client.set_object_cache(second_cache);

        // The normal constructor used by prepared statements and JSON plans
        // discovers the database's current registry, then freezes its snapshot.
        let mut earlier = Optimizer::new(Arc::clone(&first_context), OptimizerConfig::default());
        add_rule(&first, "loaded-later", &observations)?;
        earlier.optimize(plan())?;
        assert_eq!(
            observations.lock().unwrap().calls,
            [("first-database", "pre"), ("first-database", "post")]
        );
        observations.lock().unwrap().calls.clear();
        Optimizer::new(second_context, OptimizerConfig::default()).optimize(plan())?;
        assert_eq!(
            observations.lock().unwrap().calls,
            [("second-database", "pre"), ("second-database", "post")]
        );
        observations.lock().unwrap().calls.clear();
        Optimizer::new(context()?, OptimizerConfig::default()).optimize(plan())?;
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);

        // An explicit internal override still works, including an empty snapshot.
        Optimizer::new(first_context, OptimizerConfig::default())
            .with_rules(Arc::from([]))
            .optimize(plan())?;
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);
        Ok(())
    }

    #[derive(Debug)]
    struct RegisterDuringRewrite {
        registry: Weak<OptimizerRuleRegistry>,
        registered: AtomicBool,
        observations: Arc<Mutex<Observations>>,
    }

    impl OptimizerRule for RegisterDuringRewrite {
        fn name(&self) -> &'static str {
            "loads-an-extension"
        }

        fn pre_optimize(&self, _: &mut Optimizer, plan: LogicalOperator) -> Result<LogicalOperator> {
            if !self.registered.swap(true, Ordering::SeqCst) {
                add_rule(
                    &self.registry.upgrade().unwrap(),
                    "new-provider",
                    &self.observations,
                )?;
            }
            Ok(plan)
        }
    }

    #[test]
    fn registration_inside_a_callback_affects_only_a_later_optimizer() -> Result<()> {
        let registry = Arc::new(OptimizerRuleRegistry::default());
        let observations = Arc::new(Mutex::new(Observations::default()));
        registry.register(Arc::new(RegisterDuringRewrite {
            registry: Arc::downgrade(&registry),
            registered: AtomicBool::new(false),
            observations: Arc::clone(&observations),
        }))?;
        let mut optimizer =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(registry.snapshot());
        optimizer.optimize(plan())?;
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);
        let mut later =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(registry.snapshot());
        later.optimize(plan())?;
        assert_eq!(
            observations.lock().unwrap().calls,
            [("new-provider", "pre"), ("new-provider", "post")]
        );
        Ok(())
    }

    #[derive(Debug)]
    struct FailingRule;

    impl OptimizerRule for FailingRule {
        fn name(&self) -> &'static str {
            "failure"
        }

        fn pre_optimize(&self, _: &mut Optimizer, _: LogicalOperator) -> Result<LogicalOperator> {
            Err(Error::invalid_input("internal rule failure sentinel"))
        }
    }

    #[test]
    fn rule_errors_are_propagated_and_timed_without_running_later_rules() -> Result<()> {
        let registry = OptimizerRuleRegistry::default();
        let observations = Arc::new(Mutex::new(Observations::default()));
        registry.register(Arc::new(FailingRule))?;
        add_rule(&registry, "unreached", &observations)?;
        let mut optimizer =
            Optimizer::new(context()?, OptimizerConfig::default()).with_rules(registry.snapshot());
        let error = optimizer.optimize(plan()).unwrap_err();
        assert_eq!(error.raw_message(), "internal rule failure sentinel");
        assert_eq!(observations.lock().unwrap().calls, [] as [(&str, &str); 0]);
        assert_eq!(optimizer.timings().len(), 1);
        assert_eq!(optimizer.timings()[0].0, OptimizerType::Extension);
        Ok(())
    }
}
