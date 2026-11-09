//! The relations in scope of one query level (DuckDB's `BindContext`).

use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_ast::{
    ColumnRefExpression, ExpressionType, OperatorExpression, ParsedExpression, ParsedExpressionKind,
    PositionalReferenceExpression, QualifiedColumnName, StarExpression,
};
use roastedduck_common::string_util::{
    candidates_message_bytes, similarity_rating, top_n_jaro_winkler, top_n_strings,
};
use roastedduck_common::{
    ByteString, CaseInsensitiveMap, CaseInsensitiveSet, Error, ErrorKind, Result, byte_format,
};
use roastedduck_types::{LogicalType, LogicalTypeId};

use super::BindingMode;
use super::binding::{Binding, BindingAlias, BindingKind};
use crate::expression::Expression;

fn binder_error(message: impl Into<ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Binder, message.into().into_bytes())
}
fn internal_error(message: impl Into<ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Internal, message.into().into_bytes())
}

/// Columns joined with `USING`/`NATURAL` (DuckDB's `UsingColumnSet`).
#[derive(Clone, Debug, Default)]
pub struct UsingColumnSet {
    /// The binding whose column stands for all of them (outer joins have none).
    pub primary_binding: BindingAlias,
    pub bindings: Vec<BindingAlias>,
}

/// A shared using-column set: bind contexts refer to the same set by identity.
pub type UsingSetRef = Arc<Mutex<UsingColumnSet>>;

fn lock(set: &UsingSetRef) -> std::sync::MutexGuard<'_, UsingColumnSet> {
    set.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The relations, USING columns and CTEs visible at one query level.
#[derive(Clone, Debug, Default)]
pub struct BindContext {
    bindings: Vec<Binding>,
    using_columns: CaseInsensitiveMap<Vec<UsingSetRef>, ByteString>,
    cte_bindings: Vec<Binding>,
}

/// `alias`, or `schema.alias`, or the full name: the shortest name telling `alias` apart
/// from `other` (DuckDB's `MinimumUniqueAlias`).
fn minimum_unique_alias(alias: &BindingAlias, other: &BindingAlias) -> ByteString {
    if !alias.alias.eq_ignore_ascii_case(&other.alias) {
        return alias.alias.clone();
    }
    if !alias.schema.eq_ignore_ascii_case(&other.schema) {
        return byte_format!("{}.{}", alias.schema, alias.alias);
    }
    alias.to_bytes()
}

fn candidate_alias(main: &BindingAlias, new: &BindingAlias) -> ByteString {
    let mut candidate = ByteString::new();
    if !main.catalog.is_empty() && !new.catalog.is_empty() {
        candidate.push_bytes(&new.catalog);
        candidate.push(b'.');
    }
    if !main.schema.is_empty() && !new.schema.is_empty() {
        candidate.push_bytes(&new.schema);
        candidate.push(b'.');
    }
    candidate.push_bytes(&new.alias);
    candidate
}

/// The alias part of a qualified column reference (DuckDB's `GetBindingAlias(ColumnRefExpression)`).
pub fn column_ref_binding_alias(names: &[ByteString]) -> BindingAlias {
    match names {
        [catalog, schema, table, _, ..] => BindingAlias::qualified(catalog, schema, table),
        [schema, table, _] => BindingAlias::qualified("", schema, table),
        [table, ..] => BindingAlias::new(table),
        [] => BindingAlias::default(),
    }
}

impl BindContext {
    /// Every relation in scope, in the order they were added.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn bindings_mut(&mut self) -> &mut [Binding] {
        &mut self.bindings
    }

    /// The binding with table index `index`.
    pub fn binding_by_index(&self, index: u64) -> Option<&Binding> {
        self.bindings.iter().find(|binding| binding.index == index)
    }

    /// The using set joining `column_name` from `binding`, if any.
    pub fn using_binding_for(
        &self,
        column_name: impl AsRef<[u8]>,
        binding: &BindingAlias,
    ) -> Option<UsingSetRef> {
        let column_name = column_name.as_ref();
        self.using_columns
            .get(column_name)?
            .iter()
            .find(|set| lock(set).bindings.iter().any(|alias| alias == binding))
            .cloned()
    }

    /// The using set for `column_name`; an error if several sets claim it
    /// (DuckDB's `GetUsingBinding(column_name)`).
    pub fn using_binding(&self, column_name: impl AsRef<[u8]>) -> Result<Option<UsingSetRef>> {
        let column_name = column_name.as_ref();
        let Some(sets) = self.using_columns.get(column_name) else {
            return Ok(None);
        };
        if sets.len() > 1 {
            let mut error =
                byte_format!("Ambiguous column reference: column \"{column_name}\" can refer to either:\n");
            for set in sets {
                let set = lock(set);
                let mut bindings = ByteString::new();
                for alias in &set.bindings {
                    bindings.push_bytes(if bindings.is_empty() { "[" } else { ", " });
                    bindings.push_bytes(&alias.alias);
                    bindings.push(b'.');
                    bindings.push_bytes(&self.actual_column_name(alias, column_name)?);
                }
                error.push_bytes(&bindings);
                error.push(b']');
            }
            return Err(binder_error(error));
        }
        Ok(sets.first().cloned())
    }

    /// Registers `set` as joining `column_name`.
    pub fn add_using_binding(&mut self, column_name: impl AsRef<[u8]>, set: UsingSetRef) {
        let column_name = column_name.as_ref();
        match self.using_columns.get_mut(column_name) {
            Some(sets) => {
                if !sets.iter().any(|existing| Arc::ptr_eq(existing, &set)) {
                    sets.push(set);
                }
            }
            None => {
                self.using_columns
                    .insert(ByteString::from(column_name), vec![set]);
            }
        }
    }

    /// Removes `set` from the sets joining `column_name`.
    pub fn remove_using_binding(&mut self, column_name: impl AsRef<[u8]>, set: &UsingSetRef) {
        let column_name = column_name.as_ref();
        if let Some(sets) = self.using_columns.get_mut(column_name) {
            sets.retain(|existing| !Arc::ptr_eq(existing, set));
            if sets.is_empty() {
                self.using_columns.remove(column_name);
            }
        }
    }

    /// The column name as the binding spells it (DuckDB's `GetActualColumnName`).
    pub fn actual_column_name(
        &self,
        alias: &BindingAlias,
        column_name: impl AsRef<[u8]>,
    ) -> Result<ByteString> {
        let column_name = column_name.as_ref();
        let binding = self
            .binding(alias)?
            .ok_or_else(|| internal_error(byte_format!("No binding with name \"{}\"", alias.alias)))?;
        let index = binding.binding_index(column_name).ok_or_else(|| {
            internal_error(byte_format!(
                "Binding with name \"{}\" does not have a column named \"{column_name}\"",
                binding.alias_name()
            ))
        })?;
        Ok(binding.names.get(index as usize).cloned().unwrap_or_default())
    }

    /// The bindings that have a column called `column_name`.
    pub fn matching_bindings(&self, column_name: impl AsRef<[u8]>) -> Vec<&Binding> {
        let column_name = column_name.as_ref();
        self.bindings
            .iter()
            .filter(|binding| binding.has_matching_binding(column_name))
            .collect()
    }

    /// The single binding with a column `column_name` that is not part of a USING set;
    /// an error if there are several (DuckDB's `GetMatchingBinding`).
    pub fn matching_binding(
        &self,
        column_name: impl AsRef<[u8]>,
        location: Option<u64>,
    ) -> Result<Option<&Binding>> {
        let column_name = column_name.as_ref();
        let mut result: Option<&Binding> = None;
        for binding in &self.bindings {
            if self.using_binding_for(column_name, &binding.alias).is_some() {
                continue;
            }
            if binding.has_matching_binding(column_name) {
                if let Some(previous) = result {
                    return Err(binder_error(byte_format!(
                        "Ambiguous reference to column name \"{column_name}\" (use: \"{}.{column_name}\" or \
                         \"{}.{column_name}\")",
                        minimum_unique_alias(&previous.alias, &binding.alias),
                        minimum_unique_alias(&binding.alias, &previous.alias)
                    ))
                    .with_position(location));
                }
                result = Some(binding);
            }
        }
        Ok(result)
    }

    /// Up to five column names similar to `column_name`, qualified where ambiguous
    /// (DuckDB's `GetSimilarBindings`).
    pub fn similar_bindings(&self, column_name: impl AsRef<[u8]>) -> Vec<ByteString> {
        let column_name = column_name.as_ref();
        let mut scores = Vec::new();
        for binding in &self.bindings {
            for name in &binding.names {
                let score = similarity_rating(name, column_name);
                if self.matching_bindings(name).len() > 1 {
                    scores.push((byte_format!("{}.{name}", binding.alias_name()), score));
                } else {
                    scores.push((name.clone(), score));
                }
            }
        }
        top_n_strings(scores, 5, 0.5)
    }

    /// The bindings matching `alias`; `Err` (soft) with candidate tables if there are none
    /// (DuckDB's `GetBindings`).
    fn bindings_matching(&self, alias: &BindingAlias) -> std::result::Result<Vec<usize>, Error> {
        let matching: Vec<usize> = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| binding.alias.matches(alias))
            .map(|(i, _)| i)
            .collect();
        if matching.is_empty() {
            let candidates: Vec<ByteString> = self
                .bindings
                .iter()
                .map(|binding| candidate_alias(alias, &binding.alias))
                .collect();
            let main = candidate_alias(alias, alias);
            let candidates =
                candidates_message_bytes(&top_n_jaro_winkler(&candidates, &main), b"Candidate tables");
            return Err(binder_error(byte_format!(
                "Referenced table \"{main}\" not found!{candidates}"
            )));
        }
        Ok(matching)
    }

    /// `(use: a, b or c)` (DuckDB's `AmbiguityException`).
    fn ambiguity_message(&self, alias: &BindingAlias, matching: &[usize]) -> ByteString {
        let mut result = ByteString::from("(use: ");
        for (i, &index) in matching.iter().enumerate() {
            if i > 0 {
                result.push_bytes(if i + 1 == matching.len() { " or " } else { ", " });
            }
            let current = &self.bindings[index].alias;
            let mut minimum = ByteString::new();
            let mut duplicate = false;
            for (k, &other_index) in matching.iter().enumerate() {
                if k == i {
                    continue;
                }
                let other = &self.bindings[other_index].alias;
                if current == other {
                    duplicate = true;
                }
                let candidate = minimum_unique_alias(current, other);
                if candidate.len() > minimum.len() {
                    minimum = candidate;
                }
            }
            if duplicate {
                result = byte_format!(
                    "(duplicate alias \"{alias}\", explicitly alias one of the tables using \"AS my_alias\""
                );
            } else {
                result.push_bytes(&minimum);
            }
        }
        result.push(b')');
        result
    }

    /// The binding with a column `column_name` among those matching `alias`.
    ///
    /// The outer `Err` is a hard error (ambiguity); the inner one is a soft "not found" error.
    pub fn binding_with_column(
        &self,
        alias: &BindingAlias,
        column_name: impl AsRef<[u8]>,
    ) -> Result<std::result::Result<usize, Error>> {
        let column_name = column_name.as_ref();
        let matching = match self.bindings_matching(alias) {
            Ok(matching) => matching,
            Err(error) => return Ok(Err(error)),
        };
        let mut result = None;
        for &index in &matching {
            if !self.bindings[index].has_matching_binding(column_name) {
                continue;
            }
            if result.is_some() {
                return Err(binder_error(byte_format!(
                    "Ambiguous reference to table \"{alias}\" {}",
                    self.ambiguity_message(alias, &matching)
                )));
            }
            result = Some(index);
        }
        Ok(result.ok_or_else(|| self.bindings[matching[0]].column_not_found_error(column_name)))
    }

    /// The binding called `alias` (DuckDB's `GetBinding(alias, error)`): `Ok(None)` if there
    /// is none, an error if the alias is ambiguous.
    pub fn binding(&self, alias: &BindingAlias) -> Result<Option<&Binding>> {
        Ok(self
            .binding_index_for(alias)?
            .ok()
            .map(|index| &self.bindings[index]))
    }

    /// Like [`BindContext::binding`], returning the position or the soft not-found error.
    pub fn binding_index_for(&self, alias: &BindingAlias) -> Result<std::result::Result<usize, Error>> {
        let matching = match self.bindings_matching(alias) {
            Ok(matching) => matching,
            Err(error) => return Ok(Err(error)),
        };
        if matching.len() > 1 {
            return Err(binder_error(byte_format!(
                "Ambiguous reference to table \"{alias}\" {}",
                self.ambiguity_message(alias, &matching)
            )));
        }
        Ok(Ok(matching[0]))
    }

    /// Binds a qualified column reference (DuckDB's `BindContext::BindColumn`): `Ok(Err)` is
    /// a soft error.
    pub fn bind_column(
        &mut self,
        column: &ColumnRefExpression,
        alias: impl AsRef<[u8]>,
        depth: u64,
    ) -> Result<std::result::Result<Expression, Error>> {
        let alias = alias.as_ref();
        let binding_alias = column_ref_binding_alias(&column.column_names);
        let column_name = column.column_names.last().cloned().unwrap_or_default();
        let index = match self.binding_with_column(&binding_alias, &column_name)? {
            Ok(index) => index,
            Err(error) => return Ok(Err(error)),
        };
        Ok(self.bindings[index].bind(&column_name, alias, depth))
    }

    /// `(table, column)` for a positional reference `#n` (DuckDB's `BindColumn(PositionalReference)`).
    pub fn positional_column(
        &self,
        reference: &PositionalReferenceExpression,
    ) -> std::result::Result<(ByteString, ByteString), ByteString> {
        let mut total = 0usize;
        let mut position = reference.index.wrapping_sub(1) as usize;
        for binding in &self.bindings {
            if reference.index == 0 {
                return Ok((binding.alias_name().to_owned(), ByteString::from("rowid")));
            }
            if position < binding.names.len() {
                return Ok((binding.alias_name().to_owned(), binding.names[position].clone()));
            }
            total += binding.names.len();
            position -= binding.names.len();
        }
        Err(byte_format!(
            "Positional reference {} out of range (total {total} columns)",
            reference.index
        ))
    }

    /// The column a positional reference stands for (DuckDB's `PositionToColumn`).
    pub fn position_to_column(&self, reference: &PositionalReferenceExpression) -> Result<ParsedExpression> {
        let (table, column) = self.positional_column(reference).map_err(binder_error)?;
        Ok(ParsedExpression::column_ref([table, column]))
    }

    /// A column reference qualified with `alias` (DuckDB's `CreateColumnReference` with
    /// `EXPAND_GENERATED_COLUMNS`): a generated column of a table becomes its expression
    /// over the table's columns, aliased to the column name.
    pub fn create_column_reference(
        &self,
        alias: &BindingAlias,
        column_name: impl AsRef<[u8]>,
    ) -> ParsedExpression {
        let column_name = column_name.as_ref();
        if let Some(expanded) = self.expand_generated_column(alias, column_name) {
            return expanded;
        }
        self.create_column_reference_unexpanded(alias, column_name)
    }

    /// `TableBinding::ExpandGeneratedColumn`: the generated expression with its column
    /// references renamed to the binding's names and qualified with its alias.
    fn expand_generated_column(
        &self,
        alias: &BindingAlias,
        column_name: impl AsRef<[u8]>,
    ) -> Option<ParsedExpression> {
        let column_name = column_name.as_ref();
        let index = self.binding_with_column(alias, column_name).ok()?.ok()?;
        let binding = &self.bindings[index];
        let BindingKind::Table {
            entry: Some(entry), ..
        } = &binding.kind
        else {
            return None;
        };
        let roastedduck_catalog::EntryKind::Table(table) = &entry.kind else {
            return None;
        };
        let column_index = usize::try_from(binding.binding_index(column_name)?).ok()?;
        let generated = table.columns.get(column_index)?.generated.as_ref()?;
        let mut expression = generated.clone();
        let mut prefix = Vec::new();
        if !binding.alias.catalog.is_empty() {
            prefix.push(binding.alias.catalog.clone());
        }
        if !binding.alias.schema.is_empty() {
            prefix.push(binding.alias.schema.clone());
        }
        prefix.push(binding.alias.alias.clone());
        let mut qualify = QualifyGenerated {
            table,
            names: &binding.names,
            prefix: &prefix,
        };
        roastedduck_ast::visit::VisitorMut::visit_expression(&mut qualify, &mut expression);
        expression.alias = column_name.into();
        Some(expression)
    }

    /// A column reference qualified with `alias`, aliased to the column's own spelling if
    /// it differs (DuckDB's `CreateColumnReference` with `DO_NOT_EXPAND_GENERATED_COLUMNS`).
    pub fn create_column_reference_unexpanded(
        &self,
        alias: &BindingAlias,
        column_name: impl AsRef<[u8]>,
    ) -> ParsedExpression {
        let column_name = column_name.as_ref();
        let mut names = Vec::new();
        if !alias.catalog.is_empty() {
            names.push(alias.catalog.clone());
        }
        if !alias.schema.is_empty() {
            names.push(alias.schema.clone());
        }
        names.push(alias.alias.clone());
        names.push(ByteString::from(column_name));
        let mut result = ParsedExpression::column_ref(names);
        if let Ok(Ok(index)) = self.binding_with_column(alias, column_name) {
            let binding = &self.bindings[index];
            if let Some(column_index) = binding.binding_index(column_name)
                && let Some(actual) = binding.names.get(column_index as usize)
                && actual.as_bytes() != column_name
            {
                result.alias.clone_from(actual);
            }
        }
        result
    }

    /// Expands `*` / `t.*` / `struct.*` into column references (DuckDB's
    /// `GenerateAllColumnExpressions`); binding to extract table names does not verify the
    /// EXCLUDE and REPLACE lists.
    pub fn generate_all_column_expressions(
        &self,
        star: &StarExpression,
        mode: BindingMode,
    ) -> Result<Vec<ParsedExpression>> {
        if self.bindings.is_empty() {
            return Err(binder_error("* expression without FROM clause!"));
        }
        let mut list = Vec::new();
        let mut excluded_qualified: Vec<QualifiedColumnName> = Vec::new();
        let mut replaced = CaseInsensitiveSet::<ByteString>::default();
        let mut excluded = CaseInsensitiveSet::<ByteString>::default();
        let mut check_exclusion = |qualified: &QualifiedColumnName| {
            if star
                .exclude_list
                .iter()
                .any(|entry| qualified_matches(entry, qualified))
            {
                excluded_qualified.push(qualified.clone());
                return true;
            }
            false
        };
        // REPLACE and RENAME of one output column (DuckDB's `HandleRename`).
        let mut handle_rename = |qualified: &QualifiedColumnName, expr: &mut ParsedExpression| -> bool {
            if let Some(replacement) = star.replace_list.get(&qualified.column) {
                if replaced.contains(&qualified.column) {
                    return false;
                }
                let name = star
                    .replace_list
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(&qualified.column))
                    .map_or_else(|| qualified.column.clone(), |(name, _)| name.to_owned());
                *expr = replacement.clone();
                expr.alias.clone_from(&name);
                replaced.insert(name.clone());
                excluded.insert(name);
            }
            if let Some((_, rename)) = star
                .rename_list
                .iter()
                .find(|(name, _)| qualified_matches(name, qualified))
            {
                expr.alias.clone_from(rename);
            }
            true
        };
        if star.relation_name.is_empty() {
            let mut handled_sets: Vec<UsingSetRef> = Vec::new();
            for binding in &self.bindings {
                for column_name in &binding.names {
                    let qualified = qualified_column(&binding.alias, column_name);
                    if check_exclusion(&qualified) {
                        continue;
                    }
                    if let Some(set) = self.using_binding_for(column_name, &binding.alias) {
                        if handled_sets.iter().any(|handled| Arc::ptr_eq(handled, &set)) {
                            continue;
                        }
                        let set_data = lock(&set).clone();
                        let mut expr = if set_data.primary_binding.is_set() {
                            column_ref_with_alias(&set_data.primary_binding, column_name)
                        } else {
                            let children = set_data
                                .bindings
                                .iter()
                                .map(|alias| column_ref_with_alias(alias, column_name))
                                .collect();
                            ParsedExpression::new(
                                ExpressionType::OperatorCoalesce,
                                ParsedExpressionKind::Operator(OperatorExpression { children }),
                            )
                            .with_alias(column_name.clone())
                        };
                        if handle_rename(&qualified, &mut expr) {
                            list.push(expr);
                        }
                        handled_sets.push(set);
                        continue;
                    }
                    let mut expr = self.create_column_reference_unexpanded(&binding.alias, column_name);
                    if handle_rename(&qualified, &mut expr) {
                        list.push(expr);
                    }
                }
            }
        } else {
            let relation = BindingAlias::new(&star.relation_name);
            let (binding, struct_reference) = match self.binding_index_for(&relation)? {
                Ok(index) => (&self.bindings[index], false),
                Err(error) => match self.matching_binding(&star.relation_name, None)? {
                    Some(binding) => (binding, true),
                    None => return Err(error),
                },
            };
            if struct_reference {
                let column_index = binding.binding_index(&star.relation_name).unwrap_or_default() as usize;
                let column_type = binding.types.get(column_index).cloned().unwrap_or_default();
                if column_type.id() != LogicalTypeId::Struct {
                    return Err(binder_error(byte_format!(
                        "Cannot extract field from expression \"{star}\" because it is not a struct"
                    )));
                }
                for (field, _) in column_type.struct_children().unwrap_or_default() {
                    let qualified = QualifiedColumnName::column(field.clone());
                    if check_exclusion(&qualified) {
                        continue;
                    }
                    let mut expr = ParsedExpression::column_ref([
                        binding.alias_name().to_owned(),
                        star.relation_name.clone(),
                        field.clone(),
                    ]);
                    if handle_rename(&qualified, &mut expr) {
                        list.push(expr);
                    }
                }
            } else {
                for column_name in &binding.names {
                    let qualified = qualified_column(&binding.alias, column_name);
                    if check_exclusion(&qualified) {
                        continue;
                    }
                    let mut expr = self.create_column_reference_unexpanded(&binding.alias, column_name);
                    if handle_rename(&qualified, &mut expr) {
                        list.push(expr);
                    }
                }
            }
        }
        if mode.extracts_names() {
            return Ok(list);
        }
        let location = if star.relation_name.is_empty() {
            ByteString::from("FROM clause")
        } else {
            star.relation_name.clone()
        };
        for entry in &star.exclude_list {
            if !excluded_qualified
                .iter()
                .any(|seen| qualified_matches(entry, seen))
            {
                return Err(binder_error(byte_format!(
                    "Column \"{entry}\" in EXCLUDE list not found in {location}"
                )));
            }
        }
        for (name, _) in star.replace_list.iter() {
            if !excluded.contains(name) {
                return Err(binder_error(byte_format!(
                    "Column \"{name}\" in REPLACE list not found in {location}"
                )));
            }
        }
        Ok(list)
    }

    /// The names and types of every column in scope (DuckDB's `GetTypesAndNames`).
    pub fn types_and_names(&self) -> (Vec<ByteString>, Vec<LogicalType>) {
        let mut names = Vec::new();
        let mut types = Vec::new();
        for binding in &self.bindings {
            names.extend(binding.names.iter().cloned());
            types.extend(binding.types.iter().cloned());
        }
        (names, types)
    }

    /// The aliases of every relation in scope.
    pub fn binding_aliases(&self) -> Vec<BindingAlias> {
        self.bindings
            .iter()
            .map(|binding| binding.alias.clone())
            .collect()
    }

    /// Adds a relation.
    pub fn add_binding(&mut self, binding: Binding) {
        self.bindings.push(binding);
    }

    /// Adds a subquery, VALUES list or other generic relation (DuckDB's `AddGenericBinding`).
    pub fn add_generic_binding(
        &mut self,
        index: u64,
        alias: impl AsRef<[u8]>,
        names: Vec<ByteString>,
        types: Vec<LogicalType>,
    ) -> Result<()> {
        let alias = alias.as_ref();
        self.add_binding(Binding::new(
            BindingKind::Base,
            BindingAlias::new(alias),
            types,
            names,
            index,
        )?);
        Ok(())
    }

    /// Adds a subquery with its column aliases applied (DuckDB's `AddSubquery`).
    pub fn add_subquery(
        &mut self,
        index: u64,
        alias: impl AsRef<[u8]>,
        column_aliases: &[ByteString],
        names: &[ByteString],
        types: Vec<LogicalType>,
    ) -> Result<()> {
        let alias = alias.as_ref();
        let names = alias_column_names(alias, names, column_aliases)?;
        self.add_generic_binding(index, alias, names, types)
    }

    /// Adds a CTE; an error if one with the same name exists (DuckDB's `AddCTEBinding`).
    pub fn add_cte_binding(&mut self, binding: Binding) -> Result<()> {
        if self
            .cte_bindings
            .iter()
            .any(|existing| existing.alias == binding.alias)
        {
            return Err(binder_error(byte_format!(
                "Duplicate CTE binding \"{}\" in query!",
                binding.alias
            )));
        }
        self.cte_bindings.push(binding);
        Ok(())
    }

    /// The CTE called `name`.
    pub fn cte_binding_mut(&mut self, name: &BindingAlias) -> Option<&mut Binding> {
        self.cte_bindings
            .iter_mut()
            .find(|binding| binding.alias.matches(name))
    }

    /// The CTE called `name`.
    pub fn cte_binding(&self, name: &BindingAlias) -> Option<&Binding> {
        self.cte_bindings
            .iter()
            .find(|binding| binding.alias.matches(name))
    }

    /// Moves all relations and USING columns of `other` into this context (DuckDB's `AddContext`).
    pub fn add_context(&mut self, other: Self) {
        self.bindings.extend(other.bindings);
        for (name, sets) in other.using_columns {
            for set in sets {
                self.add_using_binding(&name, set);
            }
        }
    }

    /// Removes the relations called `aliases` (after binding a SEMI/ANTI join condition).
    pub fn remove_context(&mut self, aliases: &[BindingAlias]) {
        for alias in aliases {
            let mut removed = Vec::new();
            for (name, sets) in self.using_columns.iter() {
                for set in sets {
                    let mut set = lock(set);
                    set.bindings.retain(|binding| binding != alias);
                    if set.bindings.is_empty() || &set.primary_binding == alias {
                        removed.push(name.to_owned());
                    }
                }
            }
            for name in removed {
                self.using_columns.remove(&name);
            }
            self.bindings.retain(|binding| &binding.alias != alias);
        }
    }
}

fn qualified_column(alias: &BindingAlias, column: impl AsRef<[u8]>) -> QualifiedColumnName {
    let column = column.as_ref();
    QualifiedColumnName {
        catalog: alias.catalog.clone(),
        schema: alias.schema.clone(),
        table: alias.alias.clone(),
        column: ByteString::from(column),
    }
}

/// DuckDB's `QualifiedColumnEquality`: a prefix comparison, so `tbl.i` and `i` are equal but
/// `tbl.i` and `tbl2.i` are not.
pub fn qualified_matches(entry: &QualifiedColumnName, column: &QualifiedColumnName) -> bool {
    let part_matches = |a: &[u8], b: &[u8]| a.is_empty() || b.is_empty() || a.eq_ignore_ascii_case(b);
    entry.column.eq_ignore_ascii_case(&column.column)
        && part_matches(&entry.table, &column.table)
        && part_matches(&entry.schema, &column.schema)
        && part_matches(&entry.catalog, &column.catalog)
}

fn column_ref_with_alias(alias: &BindingAlias, column: impl AsRef<[u8]>) -> ParsedExpression {
    let column = column.as_ref();
    let mut names = Vec::new();
    if !alias.catalog.is_empty() {
        names.push(alias.catalog.clone());
    }
    if !alias.schema.is_empty() {
        names.push(alias.schema.clone());
    }
    names.push(alias.alias.clone());
    names.push(ByteString::from(column));
    ParsedExpression::column_ref(names)
}

/// Applies column aliases to a relation's names, deduplicating with `_1`, `_2` suffixes
/// (DuckDB's `BindContext::AliasColumnNames`).
pub fn alias_column_names(
    table_name: impl AsRef<[u8]>,
    names: &[ByteString],
    column_aliases: &[ByteString],
) -> Result<Vec<ByteString>> {
    let table_name = table_name.as_ref();
    if column_aliases.len() > names.len() {
        return Err(binder_error(byte_format!(
            "table \"{table_name}\" has {} columns available but {} columns specified",
            names.len(),
            column_aliases.len()
        )));
    }
    let mut seen = CaseInsensitiveSet::<ByteString>::default();
    let mut add = |base: &ByteString| {
        let mut name = base.clone();
        let mut index = 1;
        while seen.contains(&name) {
            name = byte_format!("{base}_{index}");
            index += 1;
        }
        seen.insert(name.clone());
        name
    };
    let mut result: Vec<ByteString> = column_aliases.iter().map(&mut add).collect();
    result.extend(names[column_aliases.len()..].iter().map(add));
    Ok(result)
}

/// Renames the column references of a generated expression to a binding's names and
/// qualifies them with its alias (`ReplaceAliases` + `BakeTableName`).
struct QualifyGenerated<'a> {
    table: &'a roastedduck_catalog::TableEntry,
    names: &'a [ByteString],
    prefix: &'a [ByteString],
}

impl roastedduck_ast::visit::VisitorMut for QualifyGenerated<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let roastedduck_ast::ParsedExpressionKind::ColumnRef(column) = &mut expression.kind
            && let [name] = column.column_names.as_slice()
        {
            let renamed = self
                .table
                .column(name)
                .and_then(|(index, _)| self.names.get(index))
                .cloned()
                .unwrap_or_else(|| name.clone());
            let mut names = self.prefix.to_vec();
            names.push(renamed);
            column.column_names = names;
        }
        roastedduck_ast::visit::walk_expression(self, expression);
    }
}
