//! SQL text of `CREATE` statements and constraints (DuckDB's `CreateInfo::ToString`
//! family, `TableCatalogEntry::ColumnsToSQL` and `Constraint::ToString`), as shown by
//! `duckdb_tables()`, `duckdb_indexes()` and friends.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::display::logical_type_to_string;
use crate::keyword::write_optionally_quoted;
use crate::{
    ColumnList, Constraint, CreateIndexInfo, CreateInfo, CreateInfoKind, CreateSequenceInfo, CreateTableInfo,
    ForeignKeyType, IndexConstraintType, OnCreateConflict, ParsedExpression, ParsedExpressionKind,
    TableColumnType,
};

/// The schema new entries go to by default (`DEFAULT_SCHEMA`).
const DEFAULT_SCHEMA: &str = "main";

/// `ParseInfo::QualifierToString`.
pub fn qualifier_to_string(catalog: &str, schema: &str, name: &str) -> String {
    let mut result = String::new();
    if !catalog.is_empty() {
        result.push_str(&write_optionally_quoted(catalog));
        result.push('.');
        if !schema.is_empty() {
            result.push_str(&write_optionally_quoted(schema));
            result.push('.');
        }
    } else if !schema.is_empty() && schema != DEFAULT_SCHEMA {
        result.push_str(&write_optionally_quoted(schema));
        result.push('.');
    }
    result.push_str(&write_optionally_quoted(name));
    result
}

impl Constraint {
    /// `Constraint::ToString`.
    pub fn to_sql(&self) -> String {
        match self {
            Self::NotNull(_) => "NOT NULL".to_owned(),
            Self::Check(check) => format!("CHECK({})", check.expression),
            Self::Unique(unique) => {
                let columns: Vec<String> = unique
                    .columns
                    .iter()
                    .map(|c| write_optionally_quoted(c))
                    .collect();
                let base = if unique.is_primary_key {
                    "PRIMARY KEY("
                } else {
                    "UNIQUE("
                };
                format!("{base}{})", columns.join(", "))
            }
            Self::ForeignKey(fk) => {
                if fk.fk_type != ForeignKeyType::FkTypeForeignKeyTable {
                    return String::new();
                }
                let columns: Vec<String> = fk.fk_columns.iter().map(|c| write_optionally_quoted(c)).collect();
                let mut base = format!("FOREIGN KEY ({}) REFERENCES ", columns.join(", "));
                if !fk.schema.is_empty() && fk.schema != DEFAULT_SCHEMA {
                    base.push_str(&fk.schema);
                    base.push('.');
                }
                base.push_str(&fk.table);
                if !fk.pk_columns.is_empty() {
                    let columns: Vec<String> =
                        fk.pk_columns.iter().map(|c| write_optionally_quoted(c)).collect();
                    base.push('(');
                    base.push_str(&columns.join(", "));
                    base.push(')');
                }
                base
            }
        }
    }
}

/// `TableCatalogEntry::ColumnsToSQL`: the parenthesised column list with inline
/// single-column constraints and the remaining constraints at the end.
pub fn columns_to_sql(columns: &ColumnList, constraints: &[Constraint]) -> String {
    let mut not_null: BTreeSet<u64> = BTreeSet::new();
    let mut unique_columns: BTreeSet<u64> = BTreeSet::new();
    let mut pk_columns: BTreeSet<u64> = BTreeSet::new();
    let mut multi_key_pks: BTreeSet<String> = BTreeSet::new();
    let mut extra = Vec::new();
    for constraint in constraints {
        match constraint {
            Constraint::NotNull(constraint) => {
                not_null.insert(constraint.index.0);
            }
            Constraint::Unique(unique) => {
                if unique.index.0 == u64::MAX {
                    if unique.is_primary_key {
                        multi_key_pks.extend(unique.columns.iter().cloned());
                    }
                    extra.push(constraint.to_sql());
                } else if unique.is_primary_key {
                    pk_columns.insert(unique.index.0);
                } else {
                    unique_columns.insert(unique.index.0);
                }
            }
            Constraint::ForeignKey(fk) => {
                if matches!(
                    fk.fk_type,
                    ForeignKeyType::FkTypeForeignKeyTable | ForeignKeyType::FkTypeSelfReferenceTable
                ) {
                    extra.push(constraint.to_sql());
                }
            }
            Constraint::Check(_) => extra.push(constraint.to_sql()),
        }
    }
    let mut out = String::from("(");
    for (index, column) in columns.columns.iter().enumerate() {
        let logical = index as u64;
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&write_optionally_quoted(&column.name));
        out.push(' ');
        out.push_str(&logical_type_to_string(&column.logical_type));
        if let Some(rustdb_types::TypeInfo::String(info)) = column.logical_type.info()
            && !info.collation.is_empty()
        {
            let _ = write!(out, " COLLATE {}", info.collation);
        }
        if column.category == TableColumnType::Generated {
            if let Some(expression) = &column.expression {
                // The generated expression is wrapped in a cast to the column type.
                let inner = match &expression.kind {
                    ParsedExpressionKind::Cast(cast) => cast.child.as_ref(),
                    _ => expression.as_ref(),
                };
                let _ = write!(out, " GENERATED ALWAYS AS({inner})");
            }
        } else if let Some(default) = &column.expression {
            let _ = write!(out, " DEFAULT({default})");
        }
        let single_key_pk = pk_columns.contains(&logical);
        let multi_key_pk = multi_key_pks.contains(&column.name);
        if not_null.contains(&logical) && !single_key_pk && !multi_key_pk {
            out.push_str(" NOT NULL");
        }
        if single_key_pk {
            out.push_str(" PRIMARY KEY");
        }
        if unique_columns.contains(&logical) {
            out.push_str(" UNIQUE");
        }
    }
    for constraint in extra {
        out.push_str(", ");
        out.push_str(&constraint);
    }
    out.push(')');
    out
}

impl CreateInfo {
    /// `CreateInfo::GetCreatePrefix`.
    fn create_prefix(&self, entry: &str) -> String {
        let mut prefix = String::from("CREATE");
        if self.on_conflict == OnCreateConflict::ReplaceOnConflict {
            prefix.push_str(" OR REPLACE");
        }
        if self.temporary {
            prefix.push_str(" TEMP");
        }
        prefix.push(' ');
        prefix.push_str(entry);
        prefix.push(' ');
        if self.on_conflict == OnCreateConflict::IgnoreOnConflict {
            prefix.push_str(" IF NOT EXISTS ");
        }
        prefix
    }

    /// The qualified name of the created entry.
    fn qualified(&self, name: &str) -> String {
        qualifier_to_string(
            if self.temporary { "" } else { &self.catalog },
            &self.schema,
            name,
        )
    }

    /// The SQL of the statement (`CreateInfo::ToString`); `None` for kinds without one.
    pub fn to_sql(&self) -> Option<String> {
        match &self.kind {
            CreateInfoKind::Table(table) => Some(self.table_to_sql(table)),
            CreateInfoKind::View(view) => {
                let mut result = self.create_prefix("VIEW");
                result.push_str(&self.qualified(&view.view_name));
                if !view.aliases.is_empty() {
                    let aliases: Vec<String> =
                        view.aliases.iter().map(|a| write_optionally_quoted(a)).collect();
                    let _ = write!(result, " ({})", aliases.join(", "));
                }
                let _ = write!(result, " AS {};", view.query);
                Some(result)
            }
            CreateInfoKind::Index(index) => Some(self.index_to_sql(index)),
            CreateInfoKind::Sequence(sequence) => Some(sequence_to_sql(sequence)),
            CreateInfoKind::Schema => Some(format!(
                "{}{};",
                self.create_prefix("SCHEMA"),
                write_optionally_quoted(&self.schema)
            )),
            CreateInfoKind::Type(info) => Some(self.type_to_sql(info)),
            CreateInfoKind::Macro(info) | CreateInfoKind::TableMacro(info) => {
                let definitions: Vec<String> = info.macros.iter().map(macro_to_sql).collect();
                Some(format!(
                    "{}{} {};",
                    self.create_prefix("MACRO"),
                    self.qualified(&info.name),
                    definitions.join(", ")
                ))
            }
            CreateInfoKind::Secret(_) => None,
        }
    }

    /// `CreateTypeInfo::ToString`.
    fn type_to_sql(&self, info: &crate::CreateTypeInfo) -> String {
        let mut result = self.create_prefix("TYPE");
        result.push_str(&self.qualified(&info.name));
        if let Some(values) = info.logical_type.enum_values() {
            result.push_str(" AS ENUM ( ");
            let quoted: Vec<String> = values.iter().map(|value| format!("'{value}'")).collect();
            result.push_str(&quoted.join(", "));
            result.push_str(" );");
        } else if info.logical_type.id() == rustdb_types::LogicalTypeId::Invalid {
            if let Some(query) = &info.query {
                let _ = write!(result, " AS ENUM ({query})");
            }
        } else {
            result.push_str(" AS ");
            result.push_str(&logical_type_to_string(&info.logical_type));
        }
        result.push(';');
        result
    }

    /// `CreateTableInfo::ToString`.
    fn table_to_sql(&self, table: &CreateTableInfo) -> String {
        let mut result = self.create_prefix("TABLE");
        result.push_str(&self.qualified(&table.table));
        if let Some(query) = &table.query {
            let names: Vec<String> = table
                .columns
                .columns
                .iter()
                .map(|column| write_optionally_quoted(&column.name))
                .collect();
            if !names.is_empty() {
                let _ = write!(result, "({})", names.join(", "));
            }
            result.push_str(&extra_options_to_sql(table));
            let _ = write!(result, " AS {query}");
        } else {
            result.push_str(&columns_to_sql(&table.columns, &table.constraints));
            result.push_str(&extra_options_to_sql(table));
            result.push(';');
        }
        result
    }

    /// `CreateIndexInfo::ToString`.
    fn index_to_sql(&self, index: &CreateIndexInfo) -> String {
        let mut result = String::from("CREATE");
        if index.constraint_type == IndexConstraintType::Unique {
            result.push_str(" UNIQUE");
        }
        result.push_str(" INDEX ");
        if self.on_conflict == OnCreateConflict::IgnoreOnConflict {
            result.push_str("IF NOT EXISTS ");
        }
        result.push_str(&write_optionally_quoted(&index.name));
        result.push_str(" ON ");
        result.push_str(&self.qualified(&index.table));
        if !index.index_type_name.is_empty() && index.index_type_name != "ART" {
            result.push_str(" USING ");
            result.push_str(&write_optionally_quoted(&index.index_type_name));
            result.push(' ');
        }
        result.push('(');
        result.push_str(&index_expressions_to_sql(index).join(", "));
        result.push(')');
        if !index.options.is_empty() {
            let options: Vec<String> = index
                .options
                .iter()
                .map(|(name, value)| format!("{name} = {}", value.to_varchar().unwrap_or_default()))
                .collect();
            let _ = write!(result, " WITH ({} )", options.join(", "));
        }
        result.push(';');
        result
    }
}

/// `CreateTableInfo::ExtraOptionsToString`.
fn extra_options_to_sql(table: &CreateTableInfo) -> String {
    let mut result = String::new();
    let join = |items: &[ParsedExpression]| {
        items
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    if !table.partition_keys.is_empty() {
        let _ = write!(result, " PARTITIONED BY ({})", join(&table.partition_keys));
    }
    if !table.sort_keys.is_empty() {
        let _ = write!(result, " SORTED BY ({})", join(&table.sort_keys));
    }
    if !table.options.is_empty() {
        let options: Vec<String> = table
            .options
            .iter()
            .map(|(name, value)| format!("'{name}'={value}"))
            .collect();
        let _ = write!(result, " WITH ({})", options.join(","));
    }
    result
}

/// `CreateIndexInfo::ExpressionsToList`: key expressions without their table qualifier;
/// anything but a plain column is parenthesised.
pub fn index_expressions_to_sql(index: &CreateIndexInfo) -> Vec<String> {
    index
        .parsed_expressions
        .iter()
        .map(|expression| {
            let mut copy = expression.clone();
            remove_table_qualification(&mut copy, &index.table);
            match &copy.kind {
                ParsedExpressionKind::ColumnRef(column) if column.column_names.len() == 1 => copy.to_string(),
                _ => format!("({copy})"),
            }
        })
        .collect()
}

/// Drops a table qualifier from column references (`RemoveTableQualificationRecursive`).
struct RemoveQualifier<'a> {
    table: &'a str,
}

impl crate::visit::VisitorMut for RemoveQualifier<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let ParsedExpressionKind::ColumnRef(column) = &mut expression.kind
            && column.column_names.len() > 1
            && column.column_names[0] == self.table
        {
            column.column_names.remove(0);
        }
        crate::visit::walk_expression(self, expression);
    }
}

/// `RemoveTableQualificationRecursive`.
fn remove_table_qualification(expression: &mut ParsedExpression, table: &str) {
    crate::visit::VisitorMut::visit_expression(&mut RemoveQualifier { table }, expression);
}

/// `SequenceCatalogEntry::ToSQL`.
pub fn sequence_to_sql(sequence: &CreateSequenceInfo) -> String {
    format!(
        "CREATE SEQUENCE {} INCREMENT BY {} MINVALUE {} MAXVALUE {} START {} {};",
        sequence.name,
        sequence.increment,
        sequence.min_value,
        sequence.max_value,
        sequence.start_value,
        if sequence.cycle { "CYCLE" } else { "NO CYCLE" }
    )
}

/// `MacroFunction::ToSQL` with the body (`ScalarMacroFunction` / `TableMacroFunction`).
fn macro_to_sql(function: &crate::MacroFunction) -> String {
    let mut parameters = Vec::with_capacity(function.parameters.len());
    for (index, parameter) in function.parameters.iter().enumerate() {
        let name = match &parameter.kind {
            ParsedExpressionKind::ColumnRef(column) => {
                column.column_names.last().cloned().unwrap_or_default()
            }
            _ => parameter.to_string(),
        };
        let mut text = name.clone();
        if let Some(ty) = function.types.get(index)
            && ty.id() != rustdb_types::LogicalTypeId::Unknown
        {
            text.push(' ');
            text.push_str(&logical_type_to_string(ty));
        }
        if let Some(default) = function.default_parameters.get(&name) {
            let _ = write!(text, " := {default}");
        }
        parameters.push(text);
    }
    let head = format!("({}) AS ", parameters.join(", "));
    match &function.kind {
        crate::MacroFunctionKind::Scalar(scalar) => format!("{head}({})", scalar.expression),
        crate::MacroFunctionKind::Table(table) => format!("{head}TABLE ({})", table.query_node),
    }
}
