//! The schema of the first sniffed file, which later files are checked against (DuckDB's
//! `csv_schema.hpp/.cpp`).

use std::collections::HashMap;

use roastedduck_types::{LogicalType, LogicalTypeId};

use crate::sniffer::SnifferResult;

/// One column of a [`CsvSchema`] (DuckDB's `CSVColumnInfo`).
#[derive(Clone, Debug)]
pub struct CsvColumnInfo {
    pub name: roastedduck_common::ByteString,
    pub ty: LogicalType,
}

/// The names and types of a CSV file (DuckDB's `CSVSchema`).
#[derive(Clone, Debug, Default)]
pub struct CsvSchema {
    pub columns: Vec<CsvColumnInfo>,
    name_idx_map: HashMap<roastedduck_common::ByteString, usize>,
    file_path: roastedduck_common::ByteString,
    rows_read: u64,
    empty: bool,
}

impl CsvSchema {
    pub fn new(
        names: &[roastedduck_common::ByteString],
        types: &[LogicalType],
        file_path: &[u8],
        rows_read: u64,
        empty: bool,
    ) -> Self {
        let mut schema = Self {
            rows_read,
            empty,
            ..Self::default()
        };
        schema.initialize(names, types, file_path);
        schema
    }

    /// `Initialize`.
    pub fn initialize(
        &mut self,
        names: &[roastedduck_common::ByteString],
        types: &[LogicalType],
        file_path: &[u8],
    ) {
        self.file_path = file_path.into();
        for (i, (name, ty)) in names.iter().zip(types).enumerate() {
            self.columns.push(CsvColumnInfo {
                name: name.clone(),
                ty: ty.clone(),
            });
            self.name_idx_map.insert(name.clone(), i);
        }
    }

    /// `CanWeCastIt`: whether values sniffed as `source` can be read as `destination`.
    pub fn can_we_cast_it(source: LogicalTypeId, destination: LogicalTypeId) -> bool {
        use LogicalTypeId as L;
        if destination == L::Varchar || source == destination {
            return true;
        }
        match source {
            L::SqlNull => true,
            L::TinyInt => matches!(
                destination,
                L::SmallInt | L::Integer | L::BigInt | L::Decimal | L::Float | L::Double
            ),
            L::SmallInt => matches!(
                destination,
                L::Integer | L::BigInt | L::Decimal | L::Float | L::Double
            ),
            L::Integer => matches!(destination, L::BigInt | L::Decimal | L::Float | L::Double),
            L::BigInt => matches!(destination, L::Decimal | L::Float | L::Double),
            L::Float => destination == L::Double,
            _ => false,
        }
    }

    /// `MergeSchemas`: widens the types to fit another file.
    pub fn merge_schemas(&mut self, other: &Self, null_padding: bool) {
        let candidates_by_specificity = [
            LogicalType::BOOLEAN,
            LogicalType::BIGINT,
            LogicalType::DOUBLE,
            LogicalType::VARCHAR,
        ];
        for (column, other_column) in self.columns.iter_mut().zip(&other.columns) {
            let this_type = column.ty.id();
            let other_type = other_column.ty.id();
            if column.ty != other_column.ty {
                if Self::can_we_cast_it(this_type, other_type) {
                    column.ty = other_column.ty.clone();
                } else if !Self::can_we_cast_it(other_type, this_type)
                    && let Some(ty) = candidates_by_specificity.iter().find(|ty| {
                        Self::can_we_cast_it(this_type, ty.id()) && Self::can_we_cast_it(other_type, ty.id())
                    })
                {
                    column.ty = ty.clone();
                }
            }
        }
        if null_padding && other.columns.len() > self.columns.len() {
            for i in self.columns.len()..other.columns.len() {
                let column = other.columns[i].clone();
                self.name_idx_map.insert(column.name.clone(), i);
                self.columns.push(column);
            }
        }
    }

    pub fn names(&self) -> Vec<roastedduck_common::ByteString> {
        self.columns.iter().map(|column| column.name.clone()).collect()
    }

    pub fn types(&self) -> Vec<LogicalType> {
        self.columns.iter().map(|column| column.ty.clone()).collect()
    }

    /// `ReplaceNullWithVarchar`.
    pub fn replace_null_with_varchar(&mut self) {
        for column in &mut self.columns {
            if column.ty.id() == LogicalTypeId::SqlNull {
                column.ty = LogicalType::VARCHAR;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// `MatchColumns`.
    pub fn match_columns(&self, other: &Self) -> bool {
        other.columns.len() == self.columns.len() || self.empty || other.empty
    }

    pub fn path(&self) -> &[u8] {
        &self.file_path
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn rows_read(&self) -> u64 {
        self.rows_read
    }

    /// `SchemasMatch`: whether a file's sniffed schema matches; `minimal_more_than_one_row`
    /// is set for results of the minimal sniffer.
    pub fn schemas_match(
        &self,
        error_message: &mut roastedduck_common::ByteString,
        sniffer_result: &mut SnifferResult,
        cur_file_path: &[u8],
        minimal_more_than_one_row: Option<bool>,
    ) -> bool {
        let mut current_schema: HashMap<&[u8], (LogicalType, usize)> = HashMap::new();
        for (i, (name, ty)) in sniffer_result
            .names
            .iter()
            .zip(&sniffer_result.return_types)
            .enumerate()
        {
            current_schema.insert(name.as_bytes(), (ty.clone(), i));
        }
        if minimal_more_than_one_row == Some(false) {
            if self
                .columns
                .iter()
                .all(|column| current_schema.contains_key(column.name.as_bytes()))
            {
                return true;
            }
            let types_match = sniffer_result.return_types.len() == self.columns.len()
                && self
                    .columns
                    .iter()
                    .zip(&sniffer_result.return_types)
                    .all(|(column, ty)| column.ty == *ty);
            if types_match {
                for (i, column) in self.columns.iter().enumerate() {
                    sniffer_result.names[i].clone_from(&column.name);
                }
                return true;
            }
        }
        let mut matches = true;
        let mut error = roastedduck_common::ByteString::from("Schema mismatch between globbed files.\n");
        let _ = roastedduck_common::byte_write!(error, "Main file schema: {}\n", self.file_path);
        let _ = roastedduck_common::byte_write!(error, "Current file: {cur_file_path}\n");
        for column in &self.columns {
            match current_schema.get(column.name.as_bytes()) {
                None => {
                    let _ = roastedduck_common::byte_write!(
                        error,
                        "Column with name: \"{}\" is missing\n",
                        column.name
                    );
                    matches = false;
                }
                Some((ty, _)) => {
                    if !Self::can_we_cast_it(ty.id(), column.ty.id()) {
                        let _ = roastedduck_common::byte_write!(
                            error,
                            "Column with name: \"{}\" is expected to have type: {}",
                            column.name,
                            column.ty
                        );
                        let _ = roastedduck_common::byte_write!(error, " But has type: {ty}\n");
                        matches = false;
                    }
                }
            }
        }
        error.push_bytes("Potential Fixes \n");
        error.push_bytes("* Consider setting union_by_name=true.\n");
        error.push_bytes("* Consider setting files_to_sniff to a higher value (e.g., files_to_sniff = -1)\n");
        if !matches {
            *error_message = error;
        }
        matches
    }
}
