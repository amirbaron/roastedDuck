//! SQL text of `CREATE` statements and constraints (DuckDB's `CreateInfo::ToString`
//! family, `TableCatalogEntry::ColumnsToSQL` and `Constraint::ToString`), as shown by
//! `duckdb_tables()`, `duckdb_indexes()` and friends.

use roastedduck_common::{byte_format as format, byte_write as write};
use std::collections::BTreeSet;

use crate::display::logical_type_to_string;
use crate::keyword::write_optionally_quoted_bytes;
fn write_optionally_quoted(text: impl AsRef<[u8]>) -> ByteString {
    write_optionally_quoted_bytes(text.as_ref())
}
use crate::{
    ColumnList, Constraint, CreateIndexInfo, CreateInfo, CreateInfoKind, CreateSequenceInfo, CreateTableInfo,
    ForeignKeyType, IndexConstraintType, OnCreateConflict, ParsedExpression, ParsedExpressionKind,
    TableColumnType,
};
use roastedduck_common::ByteString;

/// The schema new entries go to by default (`DEFAULT_SCHEMA`).
const DEFAULT_SCHEMA: &str = "main";

/// `ParseInfo::QualifierToString`.
pub fn qualifier_to_string(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    name: impl AsRef<[u8]>,
) -> ByteString {
    let (catalog, schema, name) = (catalog.as_ref(), schema.as_ref(), name.as_ref());
    let mut result = ByteString::new();
    if !catalog.is_empty() {
        result.push_bytes(write_optionally_quoted(catalog));
        result.push(b'.');
        if !schema.is_empty() {
            result.push_bytes(write_optionally_quoted(schema));
            result.push(b'.');
        }
    } else if !schema.is_empty() && schema != DEFAULT_SCHEMA.as_bytes() {
        result.push_bytes(write_optionally_quoted(schema));
        result.push(b'.');
    }
    result.push_bytes(write_optionally_quoted(name));
    result
}

impl Constraint {
    /// `Constraint::ToString`.
    pub fn to_sql(&self) -> ByteString {
        match self {
            Self::NotNull(_) => "NOT NULL".into(),
            Self::Check(check) => format!("CHECK({})", check.expression),
            Self::Unique(unique) => {
                let columns: Vec<ByteString> = unique.columns.iter().map(write_optionally_quoted).collect();
                let base = if unique.is_primary_key {
                    "PRIMARY KEY("
                } else {
                    "UNIQUE("
                };
                format!("{base}{})", ByteString::join(&columns, ", "))
            }
            Self::ForeignKey(fk) => {
                if fk.fk_type != ForeignKeyType::FkTypeForeignKeyTable {
                    return ByteString::new();
                }
                let columns: Vec<ByteString> = fk.fk_columns.iter().map(write_optionally_quoted).collect();
                let mut base = format!("FOREIGN KEY ({}) REFERENCES ", ByteString::join(&columns, ", "));
                if !fk.schema.is_empty() && fk.schema.as_bytes() != DEFAULT_SCHEMA.as_bytes() {
                    base.push_bytes(&fk.schema);
                    base.push(b'.');
                }
                base.push_bytes(&fk.table);
                if !fk.pk_columns.is_empty() {
                    let columns: Vec<ByteString> =
                        fk.pk_columns.iter().map(write_optionally_quoted).collect();
                    base.push(b'(');
                    base.push_bytes(ByteString::join(&columns, ", "));
                    base.push(b')');
                }
                base
            }
        }
    }
}

/// `TableCatalogEntry::ColumnsToSQL`: the parenthesised column list with inline
/// single-column constraints and the remaining constraints at the end.
pub fn columns_to_sql(columns: &ColumnList, constraints: &[Constraint]) -> ByteString {
    let mut not_null: BTreeSet<u64> = BTreeSet::new();
    let mut unique_columns: BTreeSet<u64> = BTreeSet::new();
    let mut pk_columns: BTreeSet<u64> = BTreeSet::new();
    let mut multi_key_pks: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut extra = Vec::new();
    for constraint in constraints {
        match constraint {
            Constraint::NotNull(constraint) => {
                not_null.insert(constraint.index.0);
            }
            Constraint::Unique(unique) => {
                if unique.index.0 == u64::MAX {
                    if unique.is_primary_key {
                        multi_key_pks.extend(unique.columns.iter().map(|name| name.as_bytes().to_vec()));
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
    let mut out = ByteString::from("(");
    for (index, column) in columns.columns.iter().enumerate() {
        let logical = index as u64;
        if index > 0 {
            out.push_bytes(", ");
        }
        out.push_bytes(write_optionally_quoted_bytes(&column.name));
        out.push(b' ');
        out.push_bytes(logical_type_to_string(&column.logical_type));
        if let Some(roastedduck_types::TypeInfo::String(info)) = column.logical_type.info()
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
        let multi_key_pk = multi_key_pks.contains(column.name.as_bytes());
        if not_null.contains(&logical) && !single_key_pk && !multi_key_pk {
            out.push_bytes(" NOT NULL");
        }
        if single_key_pk {
            out.push_bytes(" PRIMARY KEY");
        }
        if unique_columns.contains(&logical) {
            out.push_bytes(" UNIQUE");
        }
    }
    for constraint in extra {
        out.push_bytes(", ");
        out.push_bytes(&constraint);
    }
    out.push(b')');
    out
}

impl CreateInfo {
    /// `CreateInfo::GetCreatePrefix`.
    fn create_prefix(&self, entry: &str) -> ByteString {
        let mut prefix = ByteString::from("CREATE");
        if self.on_conflict == OnCreateConflict::ReplaceOnConflict {
            prefix.push_bytes(" OR REPLACE");
        }
        if self.temporary {
            prefix.push_bytes(" TEMP");
        }
        prefix.push(b' ');
        prefix.push_bytes(entry);
        prefix.push(b' ');
        if self.on_conflict == OnCreateConflict::IgnoreOnConflict {
            prefix.push_bytes(" IF NOT EXISTS ");
        }
        prefix
    }

    /// The qualified name of the created entry.
    fn qualified(&self, name: impl AsRef<[u8]>) -> ByteString {
        qualifier_to_string(
            if self.temporary {
                b"".as_slice()
            } else {
                self.catalog.as_bytes()
            },
            &self.schema,
            name,
        )
    }

    /// The SQL of the statement (`CreateInfo::ToString`); `None` for kinds without one.
    pub fn to_sql(&self) -> Option<ByteString> {
        match &self.kind {
            CreateInfoKind::Table(table) => Some(self.table_to_sql(table)),
            CreateInfoKind::View(view) => {
                let mut result = self.create_prefix("VIEW");
                result.push_bytes(self.qualified(&view.view_name));
                if !view.aliases.is_empty() {
                    let aliases: Vec<ByteString> = view.aliases.iter().map(write_optionally_quoted).collect();
                    let _ = write!(result, " ({})", ByteString::join(&aliases, ", "));
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
                let definitions: Vec<ByteString> = info.macros.iter().map(macro_to_sql).collect();
                Some(format!(
                    "{}{} {};",
                    self.create_prefix("MACRO"),
                    self.qualified(&info.name),
                    ByteString::join(&definitions, ", ")
                ))
            }
            CreateInfoKind::Secret(_) => None,
        }
    }

    /// `CreateTypeInfo::ToString`.
    fn type_to_sql(&self, info: &crate::CreateTypeInfo) -> ByteString {
        let mut result = self.create_prefix("TYPE");
        result.push_bytes(self.qualified(&info.name));
        if let Some(values) = info.logical_type.enum_values() {
            result.push_bytes(" AS ENUM ( ");
            let quoted: Vec<ByteString> = values.iter().map(|value| format!("'{value}'")).collect();
            result.push_bytes(ByteString::join(&quoted, ", "));
            result.push_bytes(" );");
        } else if info.logical_type.id() == roastedduck_types::LogicalTypeId::Invalid {
            if let Some(query) = &info.query {
                let _ = write!(result, " AS ENUM ({query})");
            }
        } else {
            result.push_bytes(" AS ");
            result.push_bytes(logical_type_to_string(&info.logical_type));
        }
        result.push(b';');
        result
    }

    /// `CreateTableInfo::ToString`.
    fn table_to_sql(&self, table: &CreateTableInfo) -> ByteString {
        let mut result = self.create_prefix("TABLE");
        result.push_bytes(self.qualified(&table.table));
        if let Some(query) = &table.query {
            let names: Vec<ByteString> = table
                .columns
                .columns
                .iter()
                .map(|column| write_optionally_quoted_bytes(&column.name))
                .collect();
            if !names.is_empty() {
                result.push(b'(');
                for (index, name) in names.iter().enumerate() {
                    if index > 0 {
                        result.push_bytes(", ");
                    }
                    result.push_bytes(name);
                }
                result.push(b')');
            }
            result.push_bytes(extra_options_to_sql(table));
            let _ = write!(result, " AS {query}");
        } else {
            result.push_bytes(columns_to_sql(&table.columns, &table.constraints));
            result.push_bytes(extra_options_to_sql(table));
            result.push(b';');
        }
        result
    }

    /// `CreateIndexInfo::ToString`.
    fn index_to_sql(&self, index: &CreateIndexInfo) -> ByteString {
        let mut result = ByteString::from("CREATE");
        if index.constraint_type == IndexConstraintType::Unique {
            result.push_bytes(" UNIQUE");
        }
        result.push_bytes(" INDEX ");
        if self.on_conflict == OnCreateConflict::IgnoreOnConflict {
            result.push_bytes("IF NOT EXISTS ");
        }
        result.push_bytes(write_optionally_quoted(&index.name));
        result.push_bytes(" ON ");
        result.push_bytes(self.qualified(&index.table));
        if !index.index_type_name.is_empty() && index.index_type_name != "ART" {
            result.push_bytes(" USING ");
            result.push_bytes(write_optionally_quoted(&index.index_type_name));
            result.push(b' ');
        }
        result.push(b'(');
        result.push_bytes(ByteString::join(&index_expressions_to_sql(index), ", "));
        result.push(b')');
        if !index.options.is_empty() {
            let options: Vec<ByteString> = index
                .options
                .iter()
                .map(|(name, value)| format!("{name} = {}", value.to_varchar().unwrap_or_default()))
                .collect();
            let _ = write!(result, " WITH ({} )", ByteString::join(&options, ", "));
        }
        result.push(b';');
        result
    }
}

/// `CreateTableInfo::ExtraOptionsToString`.
fn extra_options_to_sql(table: &CreateTableInfo) -> ByteString {
    let mut result = ByteString::new();
    let join = |items: &[ParsedExpression]| {
        ByteString::join(
            &items.iter().map(|value| format!("{value}")).collect::<Vec<_>>(),
            ",",
        )
    };
    if !table.partition_keys.is_empty() {
        let _ = write!(result, " PARTITIONED BY ({})", join(&table.partition_keys));
    }
    if !table.sort_keys.is_empty() {
        let _ = write!(result, " SORTED BY ({})", join(&table.sort_keys));
    }
    if !table.options.is_empty() {
        let options: Vec<ByteString> = table
            .options
            .iter()
            .map(|(name, value)| format!("'{name}'={value}"))
            .collect();
        let _ = write!(result, " WITH ({})", ByteString::join(&options, ","));
    }
    result
}

/// `CreateIndexInfo::ExpressionsToList`: key expressions without their table qualifier;
/// anything but a plain column is parenthesised.
pub fn index_expressions_to_sql(index: &CreateIndexInfo) -> Vec<ByteString> {
    index
        .parsed_expressions
        .iter()
        .map(|expression| {
            let mut copy = expression.clone();
            remove_table_qualification(&mut copy, &index.table);
            match &copy.kind {
                ParsedExpressionKind::ColumnRef(column) if column.column_names.len() == 1 => {
                    copy.to_sql_bytes()
                }
                _ => format!("({copy})"),
            }
        })
        .collect()
}

/// Drops a table qualifier from column references (`RemoveTableQualificationRecursive`).
struct RemoveQualifier<'a> {
    table: &'a [u8],
}

impl crate::visit::VisitorMut for RemoveQualifier<'_> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if let ParsedExpressionKind::ColumnRef(column) = &mut expression.kind
            && column.column_names.len() > 1
            && column.column_names[0].as_bytes() == self.table
        {
            column.column_names.remove(0);
        }
        crate::visit::walk_expression(self, expression);
    }
}

/// `RemoveTableQualificationRecursive`.
fn remove_table_qualification(expression: &mut ParsedExpression, table: impl AsRef<[u8]>) {
    let table = table.as_ref();
    crate::visit::VisitorMut::visit_expression(&mut RemoveQualifier { table }, expression);
}

/// `SequenceCatalogEntry::ToSQL`.
pub fn sequence_to_sql(sequence: &CreateSequenceInfo) -> ByteString {
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
fn macro_to_sql(function: &crate::MacroFunction) -> ByteString {
    let mut parameters = Vec::with_capacity(function.parameters.len());
    for (index, parameter) in function.parameters.iter().enumerate() {
        let name = match &parameter.kind {
            ParsedExpressionKind::ColumnRef(column) => {
                column.column_names.last().cloned().unwrap_or_default()
            }
            _ => parameter.to_sql_bytes(),
        };
        let mut text = name.clone();
        if let Some(ty) = function.types.get(index)
            && ty.id() != roastedduck_types::LogicalTypeId::Unknown
        {
            text.push(b' ');
            text.push_bytes(logical_type_to_string(ty));
        }
        if let Some(default) = function.default_parameters.get(&name) {
            let _ = write!(text, " := {default}");
        }
        parameters.push(text);
    }
    let head = format!("({}) AS ", ByteString::join(&parameters, ", "));
    match &function.kind {
        crate::MacroFunctionKind::Scalar(scalar) => format!("{head}({})", scalar.expression),
        crate::MacroFunctionKind::Table(table) => format!("{head}TABLE ({})", table.query_node),
    }
}
