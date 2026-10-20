//! Tables in scope (DuckDB's `Binding`, `TableBinding`, `DummyBinding`, `EntryBinding`,
//! `CTEBinding` and `BindingAlias`).

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::QueryNode;
use rustdb_ast::keyword::write_optionally_quoted;
use rustdb_catalog::CatalogEntry;
use rustdb_common::string_util::candidates_error_message;
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::LogicalType;

use super::BoundStatement;
use crate::ColumnBinding;
use crate::expression::Expression;
use crate::operator::{ColumnIndex, SharedColumnIds};

/// The name of the dummy binding holding macro parameters (DuckDB's `DummyBinding::DUMMY_NAME`).
pub const DUMMY_NAME: &str = "0_macro_parameters";

/// A possibly qualified table name `[catalog.][schema.]alias` (DuckDB's `BindingAlias`).
#[derive(Clone, Debug, Default)]
pub struct BindingAlias {
    pub catalog: String,
    pub schema: String,
    pub alias: String,
}

impl BindingAlias {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            ..Self::default()
        }
    }

    pub fn qualified(
        catalog: impl Into<String>,
        schema: impl Into<String>,
        alias: impl Into<String>,
    ) -> Self {
        Self {
            catalog: catalog.into(),
            schema: schema.into(),
            alias: alias.into(),
        }
    }

    pub fn is_set(&self) -> bool {
        !self.alias.is_empty()
    }

    /// Whether this alias is what `other` refers to: `tbl` matches `catalog.schema.tbl`,
    /// but `schema2.tbl` does not match `schema.tbl`.
    pub fn matches(&self, other: &Self) -> bool {
        (other.catalog.is_empty() || self.catalog.eq_ignore_ascii_case(&other.catalog))
            && (other.schema.is_empty() || self.schema.eq_ignore_ascii_case(&other.schema))
            && self.alias.eq_ignore_ascii_case(&other.alias)
    }
}

impl PartialEq for BindingAlias {
    fn eq(&self, other: &Self) -> bool {
        self.catalog.eq_ignore_ascii_case(&other.catalog)
            && self.schema.eq_ignore_ascii_case(&other.schema)
            && self.alias.eq_ignore_ascii_case(&other.alias)
    }
}

impl std::fmt::Display for BindingAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for part in [&self.catalog, &self.schema] {
            if !part.is_empty() {
                write!(f, "{}.", write_optionally_quoted(part))?;
            }
        }
        f.write_str(&write_optionally_quoted(&self.alias))
    }
}

/// The lazily bound query of a CTE (DuckDB's `CTEBindState`).
#[derive(Debug)]
pub struct CteBindState {
    /// The CTE's definition.
    pub definition: QueryNode,
    /// Column names given as `cte(a, b)`.
    pub aliases: Vec<String>,
    /// The bound query, once the CTE has been referenced.
    pub bound: Option<BoundStatement>,
    /// The binder that bound the query (its correlated columns are needed later).
    pub query_binder: Option<super::Binder>,
}

/// The kind-specific part of a [`Binding`].
#[derive(Clone, Debug)]
pub enum BindingKind {
    /// A subquery, VALUES list or other generic relation.
    Base,
    /// A base table or table function: tracks which of its columns are used.
    Table {
        bound_column_ids: SharedColumnIds,
        entry: Option<Arc<CatalogEntry>>,
        /// `rowid` and similar columns: `(column id, name, type)`.
        virtual_columns: Vec<(u64, String, LogicalType)>,
    },
    /// Macro parameters or lambda parameters.
    Dummy {
        /// The macro's argument expressions, by parameter.
        arguments: Vec<rustdb_ast::ParsedExpression>,
    },
    /// A view or other catalog entry.
    Entry { entry: Arc<CatalogEntry> },
    /// A common table expression; `bind_state` is set until the CTE is first referenced.
    Cte {
        can_be_referenced: bool,
        reference_count: u64,
        bind_state: Option<Arc<Mutex<CteBindState>>>,
    },
}

/// A relation in scope and its columns (DuckDB's `Binding`).
#[derive(Clone, Debug)]
pub struct Binding {
    pub kind: BindingKind,
    pub alias: BindingAlias,
    /// The table index of the relation's columns.
    pub index: u64,
    pub types: Vec<LogicalType>,
    pub names: Vec<String>,
    name_map: CaseInsensitiveMap<u64>,
    /// The catalog and schema of the binding's catalog entry (DuckDB reads them from
    /// `GetStandardEntry()->ParentCatalog()` / `ParentSchema()`), whatever its alias.
    pub entry_parent: Option<(String, String)>,
}

impl Binding {
    /// A binding; fails on duplicate column names (DuckDB's `Binding::Initialize`).
    pub fn new(
        kind: BindingKind,
        alias: BindingAlias,
        types: Vec<LogicalType>,
        names: Vec<String>,
        index: u64,
    ) -> Result<Self> {
        let mut binding = Self {
            kind,
            alias,
            index,
            types,
            names,
            name_map: CaseInsensitiveMap::default(),
            entry_parent: None,
        };
        binding.initialize()?;
        if let BindingKind::Table { virtual_columns, .. } = &binding.kind {
            for (id, name, _) in virtual_columns {
                if !binding.name_map.contains_key(name) {
                    binding.name_map.insert(name.clone(), *id);
                }
            }
        }
        Ok(binding)
    }

    fn initialize(&mut self) -> Result<()> {
        for (i, name) in self.names.iter().enumerate() {
            if self.name_map.contains_key(name) {
                return Err(Error::binder(format!(
                    "table \"{}\" has duplicate column name \"{name}\"",
                    self.alias.alias
                )));
            }
            self.name_map.insert(name.clone(), i as u64);
        }
        Ok(())
    }

    /// Macro parameter bindings (DuckDB's `DummyBinding`).
    pub fn dummy(types: Vec<LogicalType>, names: Vec<String>, dummy_name: &str) -> Result<Self> {
        Self::new(
            BindingKind::Dummy {
                arguments: Vec::new(),
            },
            BindingAlias::new(format!("{DUMMY_NAME}{dummy_name}")),
            types,
            names,
            u64::MAX,
        )
    }

    /// The alias (DuckDB's `GetAlias`).
    pub fn alias_name(&self) -> &str {
        &self.alias.alias
    }

    /// The index of `column_name` (case-insensitive).
    pub fn binding_index(&self, column_name: &str) -> Option<u64> {
        self.name_map.get(column_name).copied()
    }

    pub fn has_matching_binding(&self, column_name: &str) -> bool {
        self.binding_index(column_name).is_some()
    }

    /// The error for a column this binding lacks (DuckDB's `ColumnNotFoundError`).
    pub fn column_not_found_error(&self, column_name: &str) -> Error {
        match &self.kind {
            BindingKind::Table { .. } => {
                let candidates = candidates_error_message(&self.names, column_name, "Candidate bindings: ");
                Error::binder(format!(
                    "Table \"{}\" does not have a column named \"{column_name}\"\n{candidates}",
                    self.alias.alias
                ))
            }
            _ => Error::binder(format!(
                "Values list \"{}\" does not have a column named \"{column_name}\"",
                self.alias.alias
            )),
        }
    }

    /// Binds `column_name` (the last part of `names`) to a column reference
    /// (DuckDB's `Binding::Bind`); `alias` is the reference's alias, if any.
    pub fn bind(&mut self, column_name: &str, alias: &str, depth: u64) -> Result<Expression, Error> {
        let Some(column_index) = self.binding_index(column_name) else {
            return Err(self.column_not_found_error(column_name));
        };
        let virtual_type = match &self.kind {
            BindingKind::Table { virtual_columns, .. } => virtual_columns
                .iter()
                .find(|(id, _, _)| *id == column_index)
                .map(|(_, _, ty)| ty.clone()),
            _ => None,
        };
        let (logical_type, name) = if let Some(ty) = virtual_type {
            (
                ty,
                if alias.is_empty() {
                    column_name.to_owned()
                } else {
                    alias.to_owned()
                },
            )
        } else {
            let index = column_index as usize;
            let ty = self.types.get(index).cloned().unwrap_or_default();
            let name = if alias.is_empty() {
                self.names.get(index).cloned().unwrap_or_default()
            } else {
                alias.to_owned()
            };
            (ty, name)
        };
        let binding = match &mut self.kind {
            BindingKind::Table { bound_column_ids, .. } => {
                // The column's position among the columns the scan produces.
                let mut bound_column_ids = bound_column_ids.lock().unwrap_or_else(PoisonError::into_inner);
                let position = bound_column_ids
                    .iter()
                    .position(|id| id.index == column_index)
                    .unwrap_or_else(|| {
                        bound_column_ids.push(ColumnIndex::new(column_index));
                        bound_column_ids.len() - 1
                    });
                ColumnBinding::new(self.index, position as u64)
            }
            _ => ColumnBinding::new(self.index, column_index),
        };
        Ok(Expression::column_ref(name, logical_type, binding, depth))
    }

    /// The macro argument for a parameter reference (DuckDB's `DummyBinding::ParamToArg`).
    pub fn param_to_arg(&self, column_name: &str, alias: &str) -> Result<rustdb_ast::ParsedExpression> {
        let index = self
            .binding_index(column_name)
            .ok_or_else(|| Error::internal(format!("Column {column_name} not found in macro")))?;
        let BindingKind::Dummy { arguments } = &self.kind else {
            return Err(Error::internal(
                "parameter of a binding that is not a macro binding",
            ));
        };
        let mut argument = arguments
            .get(index as usize)
            .cloned()
            .ok_or_else(|| Error::internal(format!("Column {column_name} not found in macro")))?;
        alias.clone_into(&mut argument.alias);
        Ok(argument)
    }

    /// Records the catalog and schema the binding's catalog entry belongs to.
    #[must_use]
    pub fn with_entry_parent(mut self, catalog: &str, schema: &str) -> Self {
        self.entry_parent = Some((catalog.to_owned(), schema.to_owned()));
        self
    }

    /// The catalog entry behind a table or view binding.
    pub fn standard_entry(&self) -> Option<&Arc<CatalogEntry>> {
        match &self.kind {
            BindingKind::Table { entry, .. } => entry.as_ref(),
            BindingKind::Entry { entry } => Some(entry),
            _ => None,
        }
    }

    /// Whether this CTE binding may be referenced (not a self-reference while binding).
    pub fn can_be_referenced(&self) -> bool {
        matches!(
            &self.kind,
            BindingKind::Cte {
                can_be_referenced: true,
                ..
            }
        )
    }

    /// Whether this CTE has been referenced.
    pub fn is_referenced(&self) -> bool {
        matches!(&self.kind, BindingKind::Cte { reference_count, .. } if *reference_count > 0)
    }

    /// Sets names and types once a lazily bound CTE is known.
    pub fn set_columns(&mut self, names: Vec<String>, types: Vec<LogicalType>) -> Result<()> {
        self.names = names;
        self.types = types;
        self.name_map = CaseInsensitiveMap::default();
        self.initialize()
    }

    /// The lazily bound CTE state, if still pending.
    pub fn cte_bind_state(&self) -> Option<Arc<Mutex<CteBindState>>> {
        match &self.kind {
            BindingKind::Cte { bind_state, .. } => bind_state.clone(),
            _ => None,
        }
    }

    /// Records a reference to a CTE (DuckDB's `CTEBinding::Reference`, after binding).
    pub fn reference(&mut self) -> Result<()> {
        let pending = self.cte_bind_state();
        if let Some(state) = pending {
            let (names, types) = {
                let state = state.lock().unwrap_or_else(PoisonError::into_inner);
                let bound = state
                    .bound
                    .as_ref()
                    .ok_or_else(|| Error::internal("CTE referenced before it was bound"))?;
                (bound.names.clone(), bound.types.clone())
            };
            self.set_columns(names, types)?;
        }
        match &mut self.kind {
            BindingKind::Cte {
                can_be_referenced: true,
                reference_count,
                bind_state,
            } => {
                *bind_state = None;
                *reference_count += 1;
                Ok(())
            }
            _ => Err(Error::internal("CTE cannot be referenced!")),
        }
    }
}
