//! Computing identical subplans once (DuckDB's `CommonSubplanOptimizer`): subtrees with
//! the same canonical form (table indexes renumbered, aliases dropped) that end in an
//! aggregate or are selective joins become one materialized CTE read by CTE scans.
//!
//! DuckDB compares the binary serialization of each canonicalized operator; here the
//! canonical form of an operator (without its children) is rendered as text, and
//! subplans are identified by that text and the identities of their children's subplans.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use roastedduck_ast::{CteMaterialize, JoinType};
use roastedduck_common::{Error, Result};
use roastedduck_planner::operator::{COLUMN_IDENTIFIER_ROW_ID, ColumnIndex};
use roastedduck_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::cte_inlining::ends_in_aggregate_or_distinct;
use crate::optimizer::Optimizer;

/// Plans at most this large are always worth materializing (`CARDINALITY_THRESHOLD`).
const CARDINALITY_THRESHOLD: u64 = 2048;
/// Larger plans must be this selective (`CARDINALITY_RATIO`).
const CARDINALITY_RATIO: u64 = 2;

/// Converts the common subplans of `plan` into materialized CTEs (DuckDB's
/// `CommonSubplanOptimizer::Optimize`).
pub fn optimize_common_subplans(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    let mut finder = CommonSubplanFinder::default();
    finder.find_common_subplans(plan);
    finder.filter_subplans();
    finder.convert_subplans_to_ctes(optimizer, plan)
}

/// One occurrence of a common subplan (DuckDB's `Subplan`).
struct Subplan {
    /// The node id of the operator that was at the subplan's position when found.
    slot: u64,
    canonical_bindings: Vec<ColumnBinding>,
}

/// The occurrences of one subplan signature (DuckDB's `SubplanInfo`).
struct SubplanInfo {
    signature: usize,
    canonical_table_indexes: Vec<u64>,
    subplans: Vec<Subplan>,
    /// The position (by its operator when found) above all occurrences.
    lowest_common_ancestor: u64,
}

/// Where an operator is in the plan (DuckDB's `OperatorInfo`).
struct OperatorInfo {
    parent: u64,
    depth: usize,
    signature: Option<usize>,
}

/// A canonical subplan (DuckDB's `PlanSignature`): its operator count; identical
/// subplans share the signature.
struct Signature {
    operator_count: usize,
}

#[derive(Default)]
struct CommonSubplanFinder {
    /// Interned signatures: (canonical operator text, child signatures) to id.
    signature_ids: HashMap<(String, Vec<usize>), usize>,
    signatures: Vec<Signature>,
    /// For every table index of an operator with a signature: its column renumbering
    /// (scans: column position to table column), empty for other operators (DuckDB's
    /// `PlanSignatureTableIndexMap::table_index_map`).
    table_index_map: HashMap<u64, HashMap<u64, u64>>,
    operator_infos: HashMap<u64, OperatorInfo>,
    /// The subplans found, in discovery order (DuckDB's `subplans` map).
    subplans: Vec<SubplanInfo>,
    /// Signature to position in `subplans`.
    subplan_index: HashMap<usize, usize>,
    /// Original table index to the canonical one of its subplan.
    to_canonical_table_index: HashMap<u64, u64>,
    /// The operator now at each position, by the operator that was there when found:
    /// rewrites replace the contents of positions (DuckDB keeps references to the
    /// `unique_ptr` slots).
    slots: HashMap<u64, u64>,
}

impl CommonSubplanFinder {
    /// DuckDB's `FindCommonSubplans`: computes the signatures bottom-up below the first
    /// operator with several children.
    fn find_common_subplans(&mut self, plan: &LogicalOperator) {
        let mut root = plan;
        while let [child] = root.children.as_slice() {
            root = child;
        }
        self.operator_infos.insert(
            root.node_id(),
            OperatorInfo {
                parent: root.node_id(),
                depth: 0,
                signature: None,
            },
        );
        for child in &root.children {
            self.visit(child, root.node_id(), 1);
        }
    }

    fn visit(&mut self, op: &LogicalOperator, parent: u64, depth: usize) {
        self.operator_infos.insert(
            op.node_id(),
            OperatorInfo {
                parent,
                depth,
                signature: None,
            },
        );
        for child in &op.children {
            self.visit(child, op.node_id(), depth + 1);
        }
        let Some(signature) = self.create_plan_signature(op) else {
            return;
        };
        if let Some(info) = self.operator_infos.get_mut(&op.node_id()) {
            info.signature = Some(signature);
        }
        let table_indexes = op.table_indexes();
        if let Some(&position) = self.subplan_index.get(&signature) {
            // Another occurrence: its table indexes map onto the first one's.
            let canonical = self.subplans[position].canonical_table_indexes.clone();
            for (table_index, canonical) in table_indexes.iter().zip(canonical) {
                self.to_canonical_table_index.insert(*table_index, canonical);
            }
            let canonical_bindings = self.canonical_bindings(op);
            let lowest_common_ancestor =
                self.lowest_common_ancestor(self.subplans[position].lowest_common_ancestor, op.node_id());
            let info = &mut self.subplans[position];
            info.subplans.push(Subplan {
                slot: op.node_id(),
                canonical_bindings,
            });
            info.lowest_common_ancestor = lowest_common_ancestor;
        } else {
            let mut canonical_table_indexes = Vec::with_capacity(table_indexes.len());
            for table_index in table_indexes {
                let canonical = self.to_canonical_table_index.len() as u64;
                canonical_table_indexes.push(canonical);
                self.to_canonical_table_index.insert(table_index, canonical);
            }
            let canonical_bindings = self.canonical_bindings(op);
            self.subplan_index.insert(signature, self.subplans.len());
            self.subplans.push(SubplanInfo {
                signature,
                canonical_table_indexes,
                subplans: vec![Subplan {
                    slot: op.node_id(),
                    canonical_bindings,
                }],
                lowest_common_ancestor: op.node_id(),
            });
        }
    }

    /// DuckDB's `CreatePlanSignature`: none if a child has none or the operator cannot be
    /// materialized.
    fn create_plan_signature(&mut self, op: &LogicalOperator) -> Option<usize> {
        let mut child_signatures = Vec::with_capacity(op.children.len());
        for child in &op.children {
            child_signatures.push(self.operator_infos.get(&child.node_id())?.signature?);
        }
        let text = self.canonical_operator(op)?;
        let operator_count = 1 + child_signatures
            .iter()
            .map(|&signature| self.signatures[signature].operator_count)
            .sum::<usize>();
        let next = self.signatures.len();
        let id = *self.signature_ids.entry((text, child_signatures)).or_insert(next);
        if id == next {
            self.signatures.push(Signature { operator_count });
        }
        Some(id)
    }

    /// The canonical text of an operator without its children (DuckDB's
    /// `PlanSignature::Create`: `Convert<TO_CANONICAL>` then `Serialize`); `None` if it is
    /// not supported or cannot be materialized.
    fn canonical_operator(&mut self, op: &LogicalOperator) -> Option<String> {
        if !operator_is_supported(op) {
            return None;
        }
        // The children's tables, numbered in order of appearance.
        let mut to_canonical: HashMap<u64, u64> = HashMap::new();
        for child in &op.children {
            for binding in child.column_bindings() {
                let next = to_canonical.len() as u64;
                to_canonical.entry(binding.table_index).or_insert(next);
            }
        }
        let mut canonical = LogicalOperator::new(op.kind.clone()).with_expressions(op.expressions.clone());
        let mut can_materialize = true;

        // The operator's own table indexes follow the children's.
        let own_indexes = op.table_indexes();
        for &table_index in &own_indexes {
            self.table_index_map.entry(table_index).or_default();
        }
        let base = to_canonical.len() as u64;
        set_table_indexes(&mut canonical.kind, |position| base + position);

        // Projection maps only drop columns.
        match &mut canonical.kind {
            LogicalKind::ComparisonJoin(join) => {
                join.left_projection_map.clear();
                join.right_projection_map.clear();
            }
            LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => {
                projection_map.clear();
            }
            _ => {}
        }

        // A scan reads all columns; references name table columns.
        if let LogicalKind::Get(get) = &mut canonical.kind {
            let column_ids = std::mem::take(&mut get.column_ids);
            let projection_ids = std::mem::take(&mut get.projection_ids);
            get.column_ids = (0..get.names.len() as u64).map(ColumnIndex::new).collect();
            if get.table.is_some() {
                get.column_ids.push(ColumnIndex::new(COLUMN_IDENTIFIER_ROW_ID));
            }
            if column_ids
                .iter()
                .any(|column| column.is_row_id_column() || !column.child_indexes.is_empty())
            {
                can_materialize = false;
            } else if let Some(&table_index) = own_indexes.first() {
                let map = self.table_index_map.entry(table_index).or_default();
                if projection_ids.is_empty() {
                    for (position, column) in column_ids.iter().enumerate() {
                        map.insert(position as u64, column.index);
                    }
                } else {
                    for &projection_id in &projection_ids {
                        if let Some(column) = column_ids.get(projection_id as usize) {
                            map.insert(projection_id, column.index);
                        }
                    }
                }
            }
        }

        // References to the children's tables, without aliases or locations.
        let table_index_map = &self.table_index_map;
        let mut valid = true;
        canonical.for_each_expression_mut(|expression| {
            canonicalize_expression(
                expression,
                &to_canonical,
                table_index_map,
                &mut valid,
                &mut can_materialize,
            );
        });
        if !valid || !can_materialize {
            return None;
        }
        Some(operator_text(&canonical))
    }

    /// The canonical bindings of an operator's output (DuckDB's `GetCanonicalBindings`).
    fn canonical_bindings(&self, op: &LogicalOperator) -> Vec<ColumnBinding> {
        op.column_bindings()
            .into_iter()
            .map(|binding| {
                let table_index = self
                    .to_canonical_table_index
                    .get(&binding.table_index)
                    .copied()
                    .unwrap_or(u64::MAX);
                let column_index = self
                    .table_index_map
                    .get(&binding.table_index)
                    .filter(|map| !map.is_empty())
                    .and_then(|map| map.get(&binding.column_index).copied())
                    .unwrap_or(binding.column_index);
                ColumnBinding::new(table_index, column_index)
            })
            .collect()
    }

    /// DuckDB's `LowestCommonAncestor`, by walking the parents.
    fn lowest_common_ancestor(&self, mut a: u64, mut b: u64) -> u64 {
        let depth = |id: u64| self.operator_infos.get(&id).map_or(0, |info| info.depth);
        let parent = |id: u64| self.operator_infos.get(&id).map_or(id, |info| info.parent);
        while depth(a) > depth(b) {
            a = parent(a);
        }
        while depth(b) > depth(a) {
            b = parent(b);
        }
        while a != b {
            let (next_a, next_b) = (parent(a), parent(b));
            if next_a == a && next_b == b {
                break;
            }
            a = next_a;
            b = next_b;
        }
        a
    }

    /// DuckDB's `FilterSubplans`: drops single operators and subplans without a twin,
    /// subplans whose parent repeats as much, and subplans whose columns do not line up.
    fn filter_subplans(&mut self) {
        let mut to_remove = HashSet::new();
        for (position, info) in self.subplans.iter().enumerate() {
            let operator_count = self.signatures[info.signature].operator_count;
            if operator_count == 1 || info.subplans.len() == 1 {
                to_remove.insert(position);
                continue;
            }
            // A parent repeated as much covers this subplan.
            let parent_signature = self
                .operator_infos
                .get(&info.subplans[0].slot)
                .and_then(|operator| self.operator_infos.get(&operator.parent))
                .and_then(|operator| operator.signature);
            if let Some(parent_signature) = parent_signature
                && let Some(&parent_position) = self.subplan_index.get(&parent_signature)
            {
                let parent = &self.subplans[parent_position];
                let subplan_count = info.subplans.len() * operator_count;
                let parent_count = parent.subplans.len() * self.signatures[parent.signature].operator_count;
                if parent_count >= subplan_count {
                    to_remove.insert(position);
                }
            }
        }
        for (position, info) in self.subplans.iter_mut().enumerate() {
            if to_remove.contains(&position) {
                continue;
            }
            // The occurrence with the most columns goes first.
            let mut max_subplan = 0;
            for (index, subplan) in info.subplans.iter().enumerate() {
                if subplan.canonical_bindings.len() > info.subplans[max_subplan].canonical_bindings.len() {
                    max_subplan = index;
                }
            }
            info.subplans.swap(0, max_subplan);
            if !columns_line_up(&info.subplans) {
                to_remove.insert(position);
            }
        }
        let mut position = 0;
        self.subplans.retain(|_| {
            let keep = !to_remove.contains(&position);
            position += 1;
            keep
        });
    }

    /// The operator now at the position `slot` names.
    fn resolve(&self, slot: u64) -> u64 {
        self.slots.get(&slot).copied().unwrap_or(slot)
    }

    /// Records that the position holding `old` now holds `new`.
    fn replace_slot(&mut self, old: u64, new: u64) {
        for value in self.slots.values_mut() {
            if *value == old {
                *value = new;
            }
        }
        self.slots.insert(old, new);
    }

    /// DuckDB's `ConvertSubplansToCTEs`.
    fn convert_subplans_to_ctes(&mut self, optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
        // The smallest subplans first (a stable sort keeps the discovery order of ties).
        let mut order: Vec<usize> = (0..self.subplans.len()).collect();
        order.sort_by_key(|&position| self.signatures[self.subplans[position].signature].operator_count);
        let mut cte_number = 1;
        let mut min_cte_index = None;
        for position in order {
            if !self.should_materialize(plan, position) {
                // Earlier materializations made it not worth it.
                continue;
            }
            let cte_index = optimizer.generate_table_index();
            let cte_name = format!("__common_subplan_{cte_number}");
            cte_number += 1;
            let min_cte_index = *min_cte_index.get_or_insert(cte_index);
            self.convert_subplan(optimizer, plan, position, cte_index, &cte_name, min_cte_index)?;
        }
        Ok(())
    }

    /// Materializes one subplan as a CTE above its occurrences.
    fn convert_subplan(
        &mut self,
        optimizer: &Optimizer,
        plan: &mut LogicalOperator,
        position: usize,
        cte_index: u64,
        cte_name: &str,
        min_cte_index: u64,
    ) -> Result<()> {
        plan.resolve_operator_types()?;
        let missing = || Error::internal("CommonSubplanOptimizer: subplan not found");
        let info = &self.subplans[position];
        let paths: Vec<Vec<usize>> = info
            .subplans
            .iter()
            .map(|subplan| find_path(plan, self.resolve(subplan.slot)).ok_or_else(missing))
            .collect::<Result<_>>()?;
        let lca_path = find_path(plan, self.resolve(info.lowest_common_ancestor)).ok_or_else(missing)?;

        // The materialized columns: the primary occurrence's.
        let primary = node_at(plan, &paths[0])?;
        let types = primary.types.clone();
        let column_names: Vec<roastedduck_common::ByteString> = (1..=types.len())
            .map(|i| roastedduck_common::byte_format!("{cte_name}_col_{i}"))
            .collect();
        let primary_index: HashMap<ColumnBinding, usize> = info.subplans[0]
            .canonical_bindings
            .iter()
            .enumerate()
            .map(|(i, binding)| (*binding, i))
            .collect();

        // The CTE scans replacing the occurrences, projected to each occurrence's columns.
        let mut cte_refs = Vec::with_capacity(info.subplans.len());
        let mut replacer = ColumnBindingReplacer::default();
        for (subplan, path) in info.subplans.iter().zip(&paths) {
            let occurrence = node_at(plan, path)?;
            let mut needs_projection = subplan.canonical_bindings.len() != types.len();
            let mut cte_columns = Vec::with_capacity(subplan.canonical_bindings.len());
            for (i, binding) in subplan.canonical_bindings.iter().enumerate() {
                let column = *primary_index
                    .get(binding)
                    .ok_or_else(|| Error::internal("CommonSubplanOptimizer: unmatched subplan column"))?;
                cte_columns.push(column);
                needs_projection |= column != i;
            }
            let cte_ref_index = optimizer.generate_table_index();
            let mut cte_ref = LogicalOperator::new(LogicalKind::CteRef {
                table_index: cte_ref_index,
                cte_index,
                chunk_types: types.clone(),
                bound_columns: column_names.clone(),
                correlated_columns: 0,
                is_recurring: false,
            });
            cte_ref.estimated_cardinality = occurrence.estimated_cardinality;
            if needs_projection {
                // Keep the occurrence's own column order.
                let select_list = cte_columns
                    .iter()
                    .map(|&column| {
                        Expression::column_ref(
                            String::new(),
                            types[column].clone(),
                            ColumnBinding::new(cte_ref_index, column as u64),
                            0,
                        )
                    })
                    .collect();
                cte_ref = LogicalOperator::projection(optimizer.generate_table_index(), select_list, cte_ref);
            }
            for (old, new) in occurrence
                .column_bindings()
                .into_iter()
                .zip(cte_ref.column_bindings())
            {
                replacer
                    .replacement_bindings
                    .push(ReplacementBinding::new(old, new));
            }
            cte_refs.push(cte_ref);
        }

        // Move the primary occurrence into the CTE and put the scans in place.
        let mut materialized = None;
        let slots: Vec<u64> = info.subplans.iter().map(|subplan| subplan.slot).collect();
        for ((path, cte_ref), slot) in paths.iter().zip(cte_refs).zip(slots) {
            let target = node_at_mut(plan, path)?;
            let new_id = cte_ref.node_id();
            let old = std::mem::replace(target, cte_ref);
            let old_id = old.node_id();
            if materialized.is_none() {
                materialized = Some(old);
            }
            self.replace_slot(old_id, new_id);
            self.slots.insert(slot, new_id);
        }
        let materialized = materialized.ok_or_else(missing)?;
        let materialized_bindings = materialized.column_bindings();
        let select_list = materialized_bindings
            .iter()
            .zip(&types)
            .map(|(binding, ty)| Expression::column_ref(String::new(), ty.clone(), *binding, 0))
            .collect();
        let materialized_projection =
            LogicalOperator::projection(optimizer.generate_table_index(), select_list, materialized);

        let lca = node_at_mut(plan, &lca_path)?;
        let remainder = std::mem::replace(lca, LogicalOperator::new(LogicalKind::CrossProduct));
        let remainder_id = remainder.node_id();
        let cte = LogicalOperator::new(LogicalKind::MaterializedCte {
            ctename: cte_name.into(),
            table_index: cte_index,
            column_count: types.len() as u64,
            materialize: CteMaterialize::CteMaterializeDefault,
            correlated_columns: roastedduck_planner::binder::CorrelatedColumns::default(),
        })
        .with_children(vec![materialized_projection, remainder]);
        let cte_id = cte.node_id();
        *lca = cte;
        self.replace_slot(remainder_id, cte_id);

        // The occurrences' readers read the scans.
        replacer.visit_operator_until(plan, Some(cte_id));
        let cte = node_at_mut(plan, &lca_path)?;
        if let Some(remainder) = cte.children.get_mut(1) {
            replacer.visit_operator(remainder);
        }

        // CTEs this optimizer made must stay in creation order for the pipelines: push
        // the new one below the older ones directly under it.
        let mut current = lca_path;
        loop {
            let node = node_at_mut(plan, &current)?;
            let swap = match node.children.get(1).map(|child| &child.kind) {
                Some(LogicalKind::MaterializedCte { table_index, .. }) => {
                    *table_index >= min_cte_index && *table_index < cte_index
                }
                _ => false,
            };
            if !swap {
                break;
            }
            // node = new(m_new, old(m_old, rest)) becomes old(m_old, new(m_new, rest)).
            let mut old = node.children.swap_remove(1);
            let rest = old.children.swap_remove(1);
            let mut new = std::mem::replace(node, LogicalOperator::new(LogicalKind::CrossProduct));
            new.children.push(rest);
            let (new_id, old_id) = (new.node_id(), old.node_id());
            old.children.push(new);
            *node = old;
            for value in self.slots.values_mut() {
                if *value == new_id {
                    *value = old_id;
                } else if *value == old_id {
                    *value = new_id;
                }
            }
            current.push(1);
        }
        Ok(())
    }

    /// DuckDB's `ShouldMaterialize`: the primary occurrence ends in an aggregate or is a
    /// selective join of several tables.
    fn should_materialize(&self, plan: &LogicalOperator, position: usize) -> bool {
        let info = &self.subplans[position];
        let Some(subplan) =
            find_path(plan, self.resolve(info.subplans[0].slot)).and_then(|path| node_at(plan, &path).ok())
        else {
            return false;
        };
        ends_in_aggregate_or_distinct(subplan) || is_selective_multi_table_plan(subplan)
    }
}

/// Whether every occurrence's columns are distinct and among the primary occurrence's
/// (the bail-outs of DuckDB's `FilterSubplans`).
fn columns_line_up(subplans: &[Subplan]) -> bool {
    let mut primary = HashSet::new();
    for binding in &subplans[0].canonical_bindings {
        if !primary.insert(*binding) {
            // A nested duplicate subplan.
            return false;
        }
    }
    subplans[1..].iter().all(|subplan| {
        let mut seen = HashSet::new();
        subplan
            .canonical_bindings
            .iter()
            .all(|binding| seen.insert(*binding) && primary.contains(binding))
    })
}

/// The base tables of a subplan and the largest's estimate (DuckDB's `GetSubplanStats`).
fn subplan_stats(op: &LogicalOperator) -> (usize, u64) {
    if op.children.is_empty() {
        return (1, op.estimated_cardinality.unwrap_or(0));
    }
    op.children
        .iter()
        .map(subplan_stats)
        .fold((0, 0), |(count, max), (child_count, child_max)| {
            (count + child_count, max.max(child_max))
        })
}

/// DuckDB's `IsSelectiveMultiTablePlan`.
fn is_selective_multi_table_plan(op: &LogicalOperator) -> bool {
    let Some(estimate) = op.estimated_cardinality else {
        return false;
    };
    let (base_table_count, max_base_table_cardinality) = subplan_stats(op);
    if base_table_count < 2 {
        return false;
    }
    estimate <= CARDINALITY_THRESHOLD || estimate < max_base_table_cardinality / CARDINALITY_RATIO
}

/// DuckDB's `PlanSignature::OperatorIsSupported`.
fn operator_is_supported(op: &LogicalOperator) -> bool {
    match &op.kind {
        LogicalKind::ChunkGet { collection, .. } => {
            // Large inline data is not worth comparing.
            collection
                .iter()
                .map(roastedduck_types::DataChunk::len)
                .sum::<usize>()
                < 1000
        }
        LogicalKind::Projection { .. }
        | LogicalKind::Filter { .. }
        | LogicalKind::Aggregate(_)
        | LogicalKind::Window { .. }
        | LogicalKind::Unnest { .. }
        | LogicalKind::Limit { .. }
        | LogicalKind::Order { .. }
        | LogicalKind::TopN { .. }
        | LogicalKind::Distinct { .. }
        | LogicalKind::Pivot { .. }
        | LogicalKind::Get(_)
        | LogicalKind::ExpressionGet { .. }
        | LogicalKind::DummyScan { .. }
        | LogicalKind::AnyJoin { .. }
        | LogicalKind::CrossProduct
        | LogicalKind::PositionalJoin
        | LogicalKind::SetOperation { .. } => true,
        LogicalKind::ComparisonJoin(_) => matches!(
            op.operator_type(),
            LogicalOperatorType::LogicalComparisonJoin | LogicalOperatorType::LogicalAsofJoin
        ),
        _ => false,
    }
}

/// Renumbers the table indexes an operator creates (DuckDB's `ConvertTableIndices`).
fn set_table_indexes(kind: &mut LogicalKind, canonical: impl Fn(u64) -> u64) {
    match kind {
        LogicalKind::Get(get) => get.table_index = canonical(0),
        LogicalKind::ChunkGet { table_index, .. }
        | LogicalKind::ExpressionGet { table_index, .. }
        | LogicalKind::DummyScan { table_index }
        | LogicalKind::CteRef { table_index, .. }
        | LogicalKind::Projection { table_index }
        | LogicalKind::SetOperation { table_index, .. } => *table_index = canonical(0),
        LogicalKind::Pivot { pivot_index, .. } => *pivot_index = canonical(0),
        LogicalKind::Unnest { unnest_index } => *unnest_index = canonical(0),
        LogicalKind::Window { window_index } => *window_index = canonical(0),
        LogicalKind::Aggregate(aggregate) => {
            aggregate.group_index = canonical(0);
            aggregate.aggregate_index = canonical(1);
            if aggregate.groupings_index != u64::MAX {
                aggregate.groupings_index = canonical(2);
            }
        }
        LogicalKind::ComparisonJoin(join) if join.join_type == JoinType::Mark => {
            join.mark_index = canonical(0);
        }
        _ => {}
    }
}

/// Points an expression's column references at canonical tables and columns and drops
/// its aliases and locations (DuckDB's `ConvertExpressions`).
fn canonicalize_expression(
    expression: &mut Expression,
    to_canonical: &HashMap<u64, u64>,
    table_index_map: &HashMap<u64, HashMap<u64, u64>>,
    valid: &mut bool,
    can_materialize: &mut bool,
) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        let binding = &mut column.binding;
        match table_index_map.get(&binding.table_index) {
            Some(map) if !map.is_empty() => match map.get(&binding.column_index) {
                Some(&column_index) => binding.column_index = column_index,
                None => *valid = false,
            },
            Some(_) => {}
            None => *valid = false,
        }
        match to_canonical.get(&binding.table_index) {
            Some(&table_index) => binding.table_index = table_index,
            None => *valid = false,
        }
    }
    expression.alias.clear();
    expression.query_location = None;
    if expression.is_volatile() {
        *can_materialize = false;
    }
    expression.for_each_child_mut(|child| {
        canonicalize_expression(child, to_canonical, table_index_map, valid, can_materialize);
    });
}

/// The text standing for a canonical operator's serialization.
fn operator_text(op: &LogicalOperator) -> String {
    let mut text = String::new();
    if let LogicalKind::Get(get) = &op.kind {
        // The scanned table rather than its catalog entry.
        let _ = write!(
            text,
            "GET {} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
            get.function.name,
            get.table_schema,
            get.table.as_ref().map(|table| table.name().to_owned()),
            get.table_index,
            get.returned_types,
            get.names,
            get.column_ids,
            get.parameters,
            get.named_parameters,
            get.input_table_types,
            get.input_table_names,
            get.projected_input,
            get.ordinality_index,
            get.table_filters,
        );
        if get.table.is_none() {
            let _ = write!(text, " {:?}", get.bind_data);
        }
        let _ = write!(text, " {:?}", get.sample_options);
    } else {
        let _ = write!(text, "{:?}", op.kind);
    }
    let _ = write!(text, " {:?}", op.expressions);
    text
}

/// The child positions leading from `op` to the operator with node id `id`.
fn find_path(op: &LogicalOperator, id: u64) -> Option<Vec<usize>> {
    if op.node_id() == id {
        return Some(Vec::new());
    }
    for (index, child) in op.children.iter().enumerate() {
        if let Some(mut path) = find_path(child, id) {
            path.insert(0, index);
            return Some(path);
        }
    }
    None
}

fn node_at<'a>(op: &'a LogicalOperator, path: &[usize]) -> Result<&'a LogicalOperator> {
    let mut node = op;
    for &index in path {
        node = node
            .children
            .get(index)
            .ok_or_else(|| Error::internal("CommonSubplanOptimizer: broken path"))?;
    }
    Ok(node)
}

fn node_at_mut<'a>(op: &'a mut LogicalOperator, path: &[usize]) -> Result<&'a mut LogicalOperator> {
    let mut node = op;
    for &index in path {
        node = node
            .children
            .get_mut(index)
            .ok_or_else(|| Error::internal("CommonSubplanOptimizer: broken path"))?;
    }
    Ok(node)
}
