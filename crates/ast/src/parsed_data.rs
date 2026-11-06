//! Hand-written parse data whose in-memory form differs from the serialized one:
//! sample options, macros and views.

use rustdb_common::{CaseInsensitiveMap, Error, OrderedMap, Result};
use rustdb_serde::{DeserializeObject, Deserializer, SerializeObject, Serializer, required};
use rustdb_types::{LogicalType, Value};

use crate::{
    MacroType, ParsedExpression, ParsedExpressionKind, SampleMethod, ScalarMacroFunction, SecretPersistType,
    SelectStatement, TableMacroFunction,
};

/// `USING SAMPLE` / `TABLESAMPLE` options (DuckDB's `SampleOptions`).
#[derive(Clone, Debug, PartialEq)]
pub struct SampleOptions {
    /// A row count or a percentage.
    pub sample_size: Value,
    /// Whether `sample_size` is a percentage rather than a row count.
    pub is_percentage: bool,
    /// The sampling method (`reservoir`, `system`, `bernoulli`).
    pub method: SampleMethod,
    /// `REPEATABLE (seed)`; `None` for a random sample.
    pub seed: Option<u64>,
    /// Whether the seed was given by the user (not serialized).
    pub repeatable: bool,
}

impl SerializeObject for SampleOptions {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(100, "sample_size", &self.sample_size)?;
        serializer.write_property_with_default(101, "is_percentage", &self.is_percentage)?;
        serializer.write_property(102, "method", &self.method)?;
        // A missing seed is written as -1.
        let seed = self.seed.map_or(-1, |seed| seed as i64);
        serializer.write_property_with_default(103, "seed", &seed)
    }
}

impl DeserializeObject for SampleOptions {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let sample_size = deserializer.read_property(100, "sample_size")?;
        let is_percentage = deserializer.read_property_with_default(101, "is_percentage")?;
        let method = deserializer.read_property(102, "method")?;
        let seed: i64 = deserializer.read_property_with_default(103, "seed")?;
        Ok(Self {
            sample_size,
            is_percentage,
            method,
            seed: u64::try_from(seed).ok(),
            repeatable: false,
        })
    }
}

rustdb_serde::impl_pointee_serde!(SampleOptions);

/// The serialization version that keeps parameters with defaults in the positional
/// parameter list and stores parameter types.
const MACRO_PARAMETER_VERSION: u64 = 6;

/// A macro definition (DuckDB's `MacroFunction`).
#[derive(Clone, Debug, PartialEq)]
pub struct MacroFunction {
    /// All parameters as column references, including those with defaults.
    pub parameters: Vec<ParsedExpression>,
    /// Default values of parameters, by name.
    pub default_parameters: CaseInsensitiveMap<ParsedExpression, rustdb_common::ByteString>,
    /// Parameter types (`UNKNOWN` when untyped), one per parameter.
    pub types: Vec<LogicalType>,
    /// The body.
    pub kind: MacroFunctionKind,
}

/// The body of a macro.
#[derive(Clone, Debug, PartialEq)]
pub enum MacroFunctionKind {
    /// `CREATE MACRO m(a) AS a + 1`.
    Scalar(ScalarMacroFunction),
    /// `CREATE MACRO m(a) AS TABLE SELECT ...`.
    Table(TableMacroFunction),
}

impl MacroFunctionKind {
    /// The `MacroType` this body is serialized with.
    pub const fn macro_type(&self) -> MacroType {
        match self {
            Self::Scalar(_) => MacroType::ScalarMacro,
            Self::Table(_) => MacroType::TableMacro,
        }
    }
}

/// The name a macro parameter is known by (`ColumnRefExpression::GetName`).
fn parameter_name(parameter: &ParsedExpression) -> Option<&rustdb_common::ByteString> {
    if !parameter.alias.is_empty() {
        return Some(&parameter.alias);
    }
    match &parameter.kind {
        ParsedExpressionKind::ColumnRef(column) => column.column_names.last(),
        _ => None,
    }
}

impl MacroFunction {
    /// `GetPositionalParametersForSerialization`: older versions list only the
    /// parameters without defaults.
    fn positional_parameters(&self, serializer: &dyn Serializer) -> Vec<&ParsedExpression> {
        let all = serializer.options().should_serialize(MACRO_PARAMETER_VERSION);
        self.parameters
            .iter()
            .filter(|parameter| {
                all || !parameter_name(parameter)
                    .is_some_and(|name| self.default_parameters.contains_key(name))
            })
            .collect()
    }

    /// `FinalizeDeserialization`: older versions did not list parameters with defaults,
    /// and did not store types.
    fn finalize_deserialization(&mut self) {
        for (name, _) in self.default_parameters.iter() {
            let listed = self.parameters.iter().any(|parameter| match &parameter.kind {
                ParsedExpressionKind::ColumnRef(column) => column
                    .column_names
                    .last()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name)),
                _ => false,
            });
            if !listed {
                self.parameters.push(ParsedExpression::column_ref([name]));
            }
        }
        if self.parameters.len() != self.types.len() {
            self.types.resize(self.parameters.len(), LogicalType::UNKNOWN);
        }
    }
}

impl SerializeObject for MacroFunction {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(100, "type", &self.kind.macro_type())?;
        let positional = self.positional_parameters(serializer);
        serializer.write_property_with_default(101, "parameters", &positional)?;
        serializer.write_property_with_default(102, "default_parameters", &self.default_parameters)?;
        if serializer.should_serialize(MACRO_PARAMETER_VERSION) {
            serializer.write_property_with_explicit_default(103, "types", &self.types, &Vec::new())?;
        }
        match &self.kind {
            MacroFunctionKind::Scalar(scalar) => scalar.serialize_fields(serializer),
            MacroFunctionKind::Table(table) => table.serialize_fields(serializer),
        }
    }
}

impl DeserializeObject for MacroFunction {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let macro_type: MacroType = deserializer.read_property(100, "type")?;
        let parameters = deserializer.read_property_with_default(101, "parameters")?;
        let default_parameters = deserializer.read_property_with_default(102, "default_parameters")?;
        let types = deserializer.read_property_with_explicit_default(103, "types", Vec::new())?;
        let kind = match macro_type {
            MacroType::ScalarMacro => {
                MacroFunctionKind::Scalar(ScalarMacroFunction::deserialize_fields(deserializer)?)
            }
            MacroType::TableMacro => {
                MacroFunctionKind::Table(TableMacroFunction::deserialize_fields(deserializer)?)
            }
            MacroType::VoidMacro => {
                return Err(Error::serialization(
                    "Unsupported type for deserialization of MacroFunction!",
                ));
            }
        };
        let mut function = Self {
            parameters,
            default_parameters,
            types,
            kind,
        };
        function.finalize_deserialization();
        Ok(function)
    }
}

rustdb_serde::impl_pointee_serde!(MacroFunction);

/// `CREATE MACRO` with one or more overloads (DuckDB's `CreateMacroInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct CreateMacroInfo {
    /// The macro name.
    pub name: rustdb_common::ByteString,
    /// The overloads; never empty.
    pub macros: Vec<MacroFunction>,
}

impl SerializeObject for CreateMacroInfo {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property_with_default(200, "name", &self.name)?;
        let (first, rest) = self
            .macros
            .split_first()
            .ok_or_else(|| Error::serialization("CreateMacroInfo without a macro"))?;
        serializer.write_optional(201, "function", false, first)?;
        serializer.write_optional(202, "extra_functions", rest.is_empty(), rest)
    }
}

impl DeserializeObject for CreateMacroInfo {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let name = deserializer.read_property_with_default(200, "name")?;
        let function: Option<Box<MacroFunction>> =
            deserializer.read_property_with_default(201, "function")?;
        let extra_functions: Vec<MacroFunction> =
            deserializer.read_property_with_default(202, "extra_functions")?;
        let mut macros = vec![*required(function, "CreateMacroInfo", "function")?];
        macros.extend(extra_functions);
        Ok(Self { name, macros })
    }
}

/// The serialization version that stores view column comments by name.
const COMMENT_MAP_VERSION: u64 = 7;

/// `CREATE VIEW` (DuckDB's `CreateViewInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct CreateViewInfo {
    /// The view name.
    pub view_name: rustdb_common::ByteString,
    /// Column names given in `CREATE VIEW v(a, b)`.
    pub aliases: Vec<rustdb_common::ByteString>,
    /// Column types, once bound.
    pub types: Vec<LogicalType>,
    /// The view's query.
    pub query: Box<SelectStatement>,
    /// Column names, once bound.
    pub names: Vec<rustdb_common::ByteString>,
    /// `COMMENT ON COLUMN` values, by column name.
    pub column_comments: OrderedMap<rustdb_common::ByteString, Value>,
}

impl CreateViewInfo {
    /// Comments as a list parallel to `names` (`GetColumnCommentsList`), for old readers.
    fn column_comments_list(&self) -> Result<Vec<Value>> {
        if self.column_comments.is_empty() {
            return Ok(Vec::new());
        }
        if self.names.is_empty() {
            return Err(Error::internal(
                "Attempting to serialize column comments using the legacy format, but view is not bound",
            ));
        }
        let mut list = vec![Value::default(); self.names.len()];
        for (name, comment) in self.column_comments.iter() {
            let index = self.names.iter().position(|n| n == name).ok_or_else(|| {
                Error::internal(rustdb_common::byte_format!(
                    "While serializing comments for view \"{}\" - did not find column \"{name}\" in list of names",
                    self.view_name
                ))
            })?;
            list[index] = comment.clone();
        }
        Ok(list)
    }
}

impl SerializeObject for CreateViewInfo {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property_with_default(200, "view_name", &self.view_name)?;
        serializer.write_property_with_default(201, "aliases", &self.aliases)?;
        serializer.write_property_with_default(202, "types", &self.types)?;
        serializer.write_optional(203, "query", false, &self.query)?;
        serializer.write_property_with_default(204, "names", &self.names)?;
        if serializer.should_serialize(COMMENT_MAP_VERSION) {
            serializer.write_property_with_default(206, "column_comments_map", &self.column_comments)
        } else {
            let list = self.column_comments_list()?;
            serializer.write_property_with_default(205, "column_comments", &list)
        }
    }
}

impl DeserializeObject for CreateViewInfo {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let view_name = deserializer.read_property_with_default(200, "view_name")?;
        let aliases = deserializer.read_property_with_default(201, "aliases")?;
        let types = deserializer.read_property_with_default(202, "types")?;
        let query: Option<Box<SelectStatement>> = deserializer.read_property_with_default(203, "query")?;
        let names: Vec<rustdb_common::ByteString> = deserializer.read_property_with_default(204, "names")?;
        let list: Vec<Value> = deserializer.read_property_with_default(205, "column_comments")?;
        let mut column_comments: OrderedMap<rustdb_common::ByteString, Value> =
            deserializer.read_property_with_default(206, "column_comments_map")?;
        if !list.is_empty() {
            if !column_comments.is_empty() {
                return Err(Error::serialization(
                    "Either column_comments or column_comments_map should be provided, not both",
                ));
            }
            for (name, comment) in names.iter().zip(list) {
                if !comment.is_null() {
                    column_comments.insert(name.clone(), comment);
                }
            }
        }
        Ok(Self {
            view_name,
            aliases,
            types,
            query: required(query, "CreateViewInfo", "query")?,
            names,
            column_comments,
        })
    }
}

/// `CREATE [PERSISTENT | TEMPORARY] SECRET name [IN storage] (TYPE ..., ...)` (DuckDB's
/// `CreateSecretInfo`). The options are expressions the binder evaluates; DuckDB never
/// serializes this info.
#[derive(Clone, Debug, PartialEq)]
pub struct CreateSecretInfo {
    /// `PERSISTENT`, `TEMPORARY`, or the default.
    pub persist_type: SecretPersistType,
    /// The secret storage (`IN storage`), lower-cased, or empty.
    pub storage_type: rustdb_common::ByteString,
    /// The secret's name, lower-cased; empty names are derived from the type.
    pub name: rustdb_common::ByteString,
    /// `SCOPE ...`
    pub scope: Option<Box<ParsedExpression>>,
    /// `TYPE ...`
    pub secret_type: Option<Box<ParsedExpression>>,
    /// `PROVIDER ...`
    pub provider: Option<Box<ParsedExpression>>,
    /// The other options, by lower-cased name.
    pub options: CaseInsensitiveMap<ParsedExpression, rustdb_common::ByteString>,
}

impl crate::LoadInfo {
    /// DuckDB's `LoadInfo::ToString`; the version is metadata, omitted by C++ here.
    pub fn to_sql_bytes(&self) -> rustdb_common::ByteString {
        use rustdb_common::{ByteString, byte_write};
        let command = match self.load_type {
            crate::LoadType::Load => "LOAD",
            crate::LoadType::Install => "INSTALL",
            crate::LoadType::ForceInstall => "FORCE INSTALL",
        };
        let mut output = ByteString::new();
        let _ = byte_write!(output, "{command} '{}'", self.filename);
        if !self.repository.is_empty() {
            output.push_bytes(" FROM ");
            output.push_bytes(if self.repo_is_alias {
                crate::keyword::write_optionally_quoted_bytes(&self.repository)
            } else {
                let mut quoted = ByteString::from(b"'");
                for &byte in self.repository.as_bytes() {
                    if byte == b'\'' {
                        quoted.push(byte);
                    }
                    quoted.push(byte);
                }
                quoted.push(b'\'');
                quoted
            });
        }
        output.push(b';');
        output
    }
}
