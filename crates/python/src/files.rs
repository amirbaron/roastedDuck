//! CSV option adapters and native file ownership; parsing, scanning and COPY run in Rust.
use crate::connection::{PyConnection, lock};
use crate::{conversion, error};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyInt, PyList, PyString};
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error};
use roastedduck_types::{LogicalType, Value};
use std::sync::Arc;
const READ_OPTIONS: &[&str] = &[
    "filename",
    "hive_types",
    "hive_partitioning",
    "union_by_name",
    "hive_types_autocast",
    "header",
    "strict_mode",
    "compression",
    "dtype",
    "sep",
    "delimiter",
    "files_to_sniff",
    "names",
    "na_values",
    "skiprows",
    "parallel",
    "quotechar",
    "comment",
    "thousands",
    "escapechar",
    "encoding",
    "date_format",
    "auto_detect",
    "timestamp_format",
    "sample_size",
    "all_varchar",
    "normalize_names",
    "null_padding",
    "lineterminator",
    "max_line_size",
    "auto_type_candidates",
    "ignore_errors",
    "store_rejects",
    "rejects_table",
    "rejects_scan",
    "rejects_limit",
    "force_not_null",
    "buffer_size",
    "decimal",
    "allow_quoted_nulls",
    "columns",
];
fn invalid(py: Python<'_>, text: impl Into<String>) -> PyErr {
    error::native(py, &Error::invalid_input(text.into()))
}
fn binder(py: Python<'_>, text: impl Into<String>) -> PyErr {
    error::native(py, &Error::binder(text.into()))
}
pub(super) fn mapping(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    object.is_instance(&object.py().import("collections.abc")?.getattr("Mapping")?)
}
pub(super) fn list_like(object: &Bound<'_, PyAny>) -> PyResult<bool> {
    if object.is_instance_of::<PyString>() || object.is_instance_of::<PyBytes>() || mapping(object)? {
        return Ok(false);
    }
    object.is_instance(&object.py().import("collections.abc")?.getattr("Iterable")?)
}
fn strings(object: &Bound<'_, PyAny>, strict: bool, message: &str) -> PyResult<Vec<Value>> {
    if strict && !object.is_instance_of::<PyList>() || !strict && !list_like(object)? {
        return Err(invalid(object.py(), message));
    }
    object
        .try_iter()?
        .map(|item| {
            let item = item?;
            if !item.is_instance_of::<PyString>() {
                return Err(invalid(object.py(), message));
            }
            Ok(Value::varchar(item.extract::<String>()?))
        })
        .collect()
}
fn unknown(key: &str, py: Python<'_>) -> PyErr {
    // Preserve the frozen unordered suggestion set, including its adjacent comment/sep literals.
    let valid = [
        "header",
        "strict_mode",
        "compression",
        "commentsep",
        "delimiter",
        "files_to_sniff",
        "dtype",
        "na_values",
        "skiprows",
        "quotechar",
        "escapechar",
        "encoding",
        "parallel",
        "date_format",
        "timestamp_format",
        "sample_size",
        "all_varchar",
        "normalize_names",
        "null_padding",
        "names",
        "lineterminator",
        "columns",
        "auto_type_candidates",
        "max_line_size",
        "ignore_errors",
        "store_rejects",
        "rejects_table",
        "rejects_scan",
        "rejects_limit",
        "force_not_null",
        "buffer_size",
        "decimal",
        "allow_quoted_nulls",
        "filename",
        "hive_partitioning",
        "union_by_name",
        "hive_types",
        "hive_types_autocast",
        "thousands",
    ];
    let mut order = roastedduck_common::libcxx::UnorderedOrder::default();
    for name in valid {
        order.insert(
            roastedduck_common::libcxx::string_hash(name.as_bytes()),
            name.to_owned(),
            |a, b| a == b,
        );
    }
    let names = order
        .order()
        .iter()
        .map(|&i| order.keys()[i].clone())
        .collect::<Vec<_>>();
    let mut message = format!(
        "The methods read_csv and read_csv_auto do not have the \"{key}\" argument.\nPossible arguments as suggestions: \n"
    );
    for name in roastedduck_common::string_util::top_n_jaro_winkler(&names, key)
        .into_iter()
        .take(3)
    {
        message.push_str("* ");
        message.push_str(&name);
        message.push('\n');
    }
    invalid(py, message)
}
fn type_name(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    if let Ok(ty) = crate::udf::annotation(object) {
        Value::try_varchar(&ty.to_bytes()).map_err(|e| error::native(object.py(), &e))
    } else {
        Ok(Value::varchar(object.str()?.extract::<String>()?))
    }
}
fn typed_map(object: &Bound<'_, PyAny>, columns: bool) -> PyResult<Value> {
    let py = object.py();
    let dict = py.import("builtins")?.getattr("dict")?.call1((object,))?;
    let dict = dict.cast::<PyDict>()?;
    let mut names = Vec::new();
    let mut values = Vec::new();
    for (name, value) in dict.iter() {
        if columns && !name.is_instance_of::<PyString>() {
            return Err(binder(
                py,
                format!(
                    "The provided column name must be a str, not of type '{}'",
                    name.get_type().str()?
                ),
            ));
        }
        if columns && !value.is_instance_of::<PyString>() {
            return Err(binder(
                py,
                format!(
                    "The provided column type must be a str, not of type '{}'",
                    name.get_type().str()?
                ),
            ));
        }
        names.push((name.str()?.extract::<String>()?.into(), LogicalType::VARCHAR));
        values.push(if columns {
            Value::varchar(value.extract::<String>()?)
        } else {
            type_name(&value)?
        });
    }
    Ok(Value::from_children(LogicalType::struct_type(names), values))
}
fn read_options(py: Python<'_>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Vec<(ByteString, Value)>> {
    let kwargs = kwargs.cloned().unwrap_or_else(|| PyDict::new(py));
    let mut result = CaseInsensitiveMap::<Value, ByteString>::default();
    let get = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(kwargs.get_item(name)?.filter(|v| !v.is_none()))
    };
    for &name in READ_OPTIONS {
        if name == "sep" && get("sep")?.is_some() && get("delimiter")?.is_some() {
            return Err(invalid(
                py,
                "read_csv takes either 'delimiter' or 'sep', not both",
            ));
        }
        let Some(value) = get(name)? else { continue };
        let mut key = name;
        let val = match name {
            "header" | "strict_mode" | "auto_detect" => {
                let enabled = if value.is_instance_of::<PyBool>() {
                    value.extract()?
                } else if value.is_instance_of::<PyInt>() {
                    if value.extract::<i64>()? != 0 {
                        return Err(invalid(
                            py,
                            format!("read_csv only accepts 0 if '{name}' is given as an integer"),
                        ));
                    }
                    true
                } else {
                    return Err(invalid(
                        py,
                        format!("read_csv only accepts '{name}' as an integer, or a boolean"),
                    ));
                };
                Value::boolean(enabled)
            }
            "compression" | "quotechar" | "escapechar" | "date_format" | "timestamp_format" | "comment"
            | "thousands" => {
                if !value.is_instance_of::<PyString>() {
                    return Err(invalid(py, format!("read_csv only accepts '{name}' as a string")));
                }
                key = match name {
                    "quotechar" => "quote",
                    "escapechar" => "escape",
                    "date_format" => "dateformat",
                    "timestamp_format" => "timestampformat",
                    _ => name,
                };
                Value::varchar(value.extract::<String>()?)
            }
            "encoding" => {
                if !value.is_instance_of::<PyString>() {
                    return Err(invalid(py, "read_csv only accepts 'encoding' as a string"));
                }
                if !["utf8", "utf-8"].contains(&value.extract::<String>()?.to_ascii_lowercase().as_str()) {
                    return Err(binder(
                        py,
                        "Copy is only supported for UTF-8 encoded files, ENCODING 'UTF-8'",
                    ));
                }
                continue;
            }
            "sep" | "delimiter" => {
                key = "delim";
                Value::varchar(value.str()?.extract::<String>()?)
            }
            "dtype" => {
                key = "dtypes";
                if mapping(&value)? {
                    typed_map(&value, false)?
                } else if list_like(&value)? {
                    Value::list(
                        LogicalType::VARCHAR,
                        value
                            .try_iter()?
                            .map(|v| type_name(&v?))
                            .collect::<PyResult<_>>()?,
                    )
                } else {
                    return Err(invalid(
                        py,
                        "read_csv only accepts 'dtype' as a dictionary or a list of strings",
                    ));
                }
            }
            "names" => {
                if !list_like(&value)? {
                    return Err(invalid(py, "read_csv only accepts 'names' as a list of strings"));
                }
                Value::list(
                    LogicalType::VARCHAR,
                    strings(
                        &value,
                        false,
                        "read_csv 'names' list has to consist of only strings",
                    )?,
                )
            }
            "na_values" => {
                key = "nullstr";
                if value.is_instance_of::<PyString>() {
                    Value::list(
                        LogicalType::VARCHAR,
                        vec![Value::varchar(value.extract::<String>()?)],
                    )
                } else {
                    if !list_like(&value)? {
                        return Err(invalid(
                            py,
                            "read_csv only accepts 'na_values' as a string or a list of strings",
                        ));
                    }
                    Value::list(
                        LogicalType::VARCHAR,
                        strings(
                            &value,
                            false,
                            "read_csv 'na_values' list has to consist of only strings",
                        )?,
                    )
                }
            }
            "files_to_sniff" | "skiprows" | "sample_size" => {
                if !value.is_instance_of::<PyInt>() {
                    return Err(invalid(
                        py,
                        format!("read_csv only accepts '{name}' as an integer"),
                    ));
                }
                if name == "skiprows" {
                    key = "skip";
                }
                Value::integer(value.extract()?)
            }
            "parallel" | "all_varchar" | "normalize_names" | "null_padding" => {
                if !value.is_instance_of::<PyBool>() {
                    return Err(invalid(
                        py,
                        format!("read_csv only accepts '{name}' as a boolean"),
                    ));
                }
                Value::boolean(value.extract()?)
            }
            "lineterminator" => {
                let text = if value.is_instance_of::<PyString>() {
                    value.extract::<String>()?
                } else if value.is_instance(&py.import("_duckdb")?.getattr("CSVLineTerminator")?)? {
                    match value.extract::<u8>()? {
                        0 => "LINE_FEED".into(),
                        1 => "CARRIAGE_RETURN_LINE_FEED".into(),
                        _ => {
                            return Err(binder(
                                py,
                                format!(
                                    "read_csv only accepts 'lineterminator' as a string or CSVLineTerminator, not '{}'",
                                    value.get_type().str()?
                                ),
                            ));
                        }
                    }
                } else {
                    return Err(binder(
                        py,
                        format!(
                            "read_csv only accepts 'lineterminator' as a string or CSVLineTerminator, not '{}'",
                            value.get_type().str()?
                        ),
                    ));
                };
                key = "new_line";
                Value::varchar(match text.as_str() {
                    "\\n" | "LINE_FEED" => "\\n",
                    "\\r\\n" | "CARRIAGE_RETURN_LINE_FEED" => "\\r\\n",
                    _ => {
                        return Err(invalid(
                            py,
                            format!("'{text}' is not a recognized type for 'lineterminator'"),
                        ));
                    }
                })
            }
            "columns" => {
                if !mapping(&value)? {
                    return Err(binder(py, "read_csv only accepts 'columns' as a dict[str, str]"));
                }
                typed_map(&value, true)?
            }
            "filename" | "hive_types" => conversion::from_python(&value)?,
            "hive_partitioning" | "union_by_name" | "hive_types_autocast" => {
                if !value.is_instance_of::<PyBool>() {
                    return Err(binder(
                        py,
                        format!(
                            "read_json only accepts '{name}' as a boolean, not '{}'",
                            value.get_type().str()?
                        ),
                    ));
                }
                Value::boolean(value.extract()?)
            }
            "auto_type_candidates" | "force_not_null" => {
                if !value.is_instance_of::<PyList>() {
                    return Err(binder(
                        py,
                        format!(
                            "read_csv only accepts '{name}' as a list[str], not '{}'",
                            value.get_type().str()?
                        ),
                    ));
                }
                let values = value
                    .try_iter()?
                    .map(|v| Ok(Value::varchar(v?.str()?.extract::<String>()?)))
                    .collect::<PyResult<Vec<_>>>()?;
                Value::list(LogicalType::VARCHAR, values)
            }
            "max_line_size" => {
                if !value.is_instance_of::<PyString>() && !value.is_instance_of::<PyInt>() {
                    return Err(binder(
                        py,
                        format!(
                            "read_csv only accepts 'max_line_size' as a string or an integer, not '{}'",
                            value.get_type().str()?
                        ),
                    ));
                }
                Value::varchar(value.str()?.extract::<String>()?)
            }
            "ignore_errors" | "store_rejects" | "allow_quoted_nulls" => {
                if !value.is_instance_of::<PyBool>() {
                    return Err(binder(
                        py,
                        format!(
                            "read_csv only accepts '{name}' as a bool, not '{}'",
                            value.get_type().str()?
                        ),
                    ));
                }
                Value::boolean(value.extract()?)
            }
            "rejects_table" | "rejects_scan" | "decimal" => {
                if !value.is_instance_of::<PyString>() {
                    return Err(binder(
                        py,
                        format!(
                            "read_csv only accepts '{name}' as a string, not '{}'",
                            value.get_type().str()?
                        ),
                    ));
                }
                if name == "decimal" {
                    key = "decimal_separator";
                }
                Value::varchar(value.extract::<String>()?)
            }
            "rejects_limit" | "buffer_size" => {
                if !value.is_instance_of::<PyInt>() {
                    return Err(binder(
                        py,
                        format!(
                            "read_csv only accepts '{name}' as a {}, not '{}'",
                            if name == "buffer_size" { "list[str]" } else { "int" },
                            value.get_type().str()?
                        ),
                    ));
                }
                if name == "buffer_size" {
                    Value::ubigint(value.extract()?)
                } else {
                    Value::bigint(value.extract()?)
                }
            }
            _ => unreachable!("every CSV option is handled"),
        };
        result.insert(key, val);
    }
    Ok(result.into_iter().collect())
}

pub(super) fn paths(
    py: Python<'_>,
    state: &std::sync::Mutex<crate::connection::State>,
    name: &Bound<'_, PyAny>,
) -> PyResult<(
    Vec<ByteString>,
    Vec<Arc<roastedduck_function::object_store::MemoryFile>>,
)> {
    let cache = py
        .detach(|| {
            lock(state)
                .reader_handle()
                .map(|c| lock(&c).function_client().object_cache())
        })
        .map_err(|e| error::native(py, &e))?
        .ok_or_else(|| error::InternalException::new_err("Missing database object cache"))?;
    let mut paths = Vec::new();
    let mut owners = Vec::new();
    let objects = if let Ok(list) = name.cast::<PyList>() {
        list.iter().collect::<Vec<_>>()
    } else {
        vec![name.clone()]
    };
    for object in objects {
        let path = if object.is_instance_of::<PyString>() {
            object.extract::<String>()?
        } else if object.is_instance_of::<PyBytes>() || object.hasattr("__fspath__")? {
            py.import("os")?
                .call_method1("fsdecode", (&object,))?
                .extract::<String>()?
        } else {
            roastedduck_function::object_store::ObjectStore::initialize(&cache);
            if !object.hasattr("read")? || !object.hasattr("seek")? {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "Can not read from a non file-like object",
                ));
            }
            let source = if object.is_instance(&py.import("io")?.getattr("TextIOBase")?)? {
                py.import("duckdb.bytes_io_wrapper")?
                    .getattr("BytesIOWrapper")?
                    .call1((&object,))?
            } else {
                object
            };
            let data = source.call_method0("read")?;
            let memory = py.import("io")?.getattr("BytesIO")?.call1((data,))?;
            let bytes = memory.call_method0("getvalue")?;
            let bytes = bytes.cast::<PyBytes>()?;
            let path = format!(
                "DUCKDB_INTERNAL_OBJECTSTORE://{:.16}",
                py.import("uuid")?
                    .call_method0("uuid4")?
                    .getattr("hex")?
                    .extract::<String>()?
            );
            owners.push(roastedduck_function::object_store::ObjectStore::register(
                &cache,
                path.as_bytes().into(),
                Arc::from(bytes.as_bytes()),
            ));
            path
        };
        paths.push(ByteString::from(path));
    }
    if paths.is_empty() {
        return Err(invalid(
            py,
            "Please provide a non-empty list of paths or file-like objects",
        ));
    }
    Ok((paths, owners))
}

pub(super) fn read(
    owner: Bound<'_, PyConnection>,
    name: &Bound<'_, PyAny>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<crate::relation::PyRelation> {
    let py = owner.py();
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            let key = key.extract::<String>()?;
            if !READ_OPTIONS.contains(&key.as_str()) {
                return Err(unknown(&key, py));
            }
        }
    }
    let state = Arc::clone(&owner.get().state);
    let (paths, owners) = paths(py, &state, name)?;
    let parameters = read_options(py, kwargs)?;
    let parameters = py
        .detach(|| {
            lock(&state)
                .connection()?
                .csv_relation_options(&paths, &parameters)
        })
        .map_err(|e| error::native(py, &e))?;
    let input = if paths.len() == 1 {
        Value::try_varchar(&paths[0])
    } else {
        Ok(Value::list(
            LogicalType::VARCHAR,
            paths
                .iter()
                .map(|p| Value::try_varchar(p))
                .collect::<roastedduck_common::Result<_>>()
                .map_err(|e| error::native(py, &e))?,
        ))
    }
    .map_err(|e| error::native(py, &e))?;
    let mut children = vec![roastedduck_parser::build::constant(input)];
    for (key, value) in parameters.parameters {
        let mut child = roastedduck_parser::build::constant(value);
        child.alias = key;
        children.push(child);
    }
    let alias = paths[0]
        .split(|&c| c == b'.')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .into();
    let mut table = roastedduck_parser::build::table_ref(roastedduck_ast::TableRefKind::TableFunction(
        roastedduck_ast::TableFunctionRef {
            function: Box::new(roastedduck_parser::build::function("read_csv_auto", children)),
            column_name_alias: Vec::new(),
            with_ordinality: roastedduck_ast::OrdinalityType::WithoutOrdinality,
        },
    ));
    table.alias = alias;
    table.external_dependency = roastedduck_ast::ExternalDependency::new(Arc::new(owners));
    crate::relation::PyRelation::from_file_table(owner, table, parameters.names, parameters.types)
}

pub(super) fn write_options(
    py: Python<'_>,
    options: &[(&str, Option<&Bound<'_, PyAny>>)],
) -> PyResult<CaseInsensitiveMap<Vec<Value>, ByteString>> {
    let mut result = CaseInsensitiveMap::default();
    for &(name, value) in options {
        let Some(value) = value.filter(|v| !v.is_none()) else {
            continue;
        };
        let key = match name {
            "sep" => "delimiter",
            "na_rep" => "null",
            "quotechar" => "quote",
            "escapechar" => "escape",
            "date_format" => "dateformat",
            "timestamp_format" => "timestampformat",
            "overwrite" => "overwrite_or_ignore",
            _ => name,
        };
        let data = match name {
            "header" | "overwrite" | "per_thread_output" | "use_tmp_file" | "write_partition_columns" => {
                if !value.is_instance_of::<PyBool>() {
                    return Err(invalid(py, format!("to_csv only accepts '{name}' as a boolean")));
                }
                vec![Value::boolean(value.extract()?)]
            }
            "quoting" => {
                if value.is_instance_of::<PyString>() {
                    if !["force", "all"].contains(&value.extract::<String>()?.to_ascii_lowercase().as_str()) {
                        return Err(invalid(
                            py,
                            "to_csv 'quoting' supported options are ALL or FORCE (both set FORCE_QUOTE=True)",
                        ));
                    }
                } else if value.is_instance_of::<PyInt>() {
                    if value.extract::<i64>()? != 1 {
                        return Err(invalid(
                            py,
                            "Only csv.QUOTE_ALL is a supported option for 'quoting' currently",
                        ));
                    }
                } else {
                    return Err(invalid(
                        py,
                        "to_csv only accepts 'quoting' as a string or a constant from the 'csv' package",
                    ));
                }
                result.insert("force_quote", vec![Value::varchar("*")]);
                continue;
            }
            "partition_by" => strings(
                value,
                true,
                "to_csv only accepts 'partition_by' as a list of strings",
            )?,
            _ => {
                if !value.is_instance_of::<PyString>() {
                    return Err(invalid(py, format!("to_csv only accepts '{name}' as a string")));
                }
                if name == "encoding" {
                    if !["utf8", "utf-8"].contains(&value.extract::<String>()?.to_ascii_lowercase().as_str())
                    {
                        return Err(invalid(py, "The only supported encoding option is 'UTF8"));
                    }
                    continue;
                }
                vec![Value::varchar(value.extract::<String>()?)]
            }
        };
        result.insert(key, data);
    }
    Ok(result)
}

/// A catalog view stores file names; the frozen CSV relation keeps ephemeral file
/// ownership on the relation, unlike the Arrow function's captured dependencies.
pub(super) fn view_source(mut source: roastedduck_ast::SelectStatement) -> roastedduck_ast::SelectStatement {
    use roastedduck_ast::visit::VisitorMut;
    struct Remove;
    impl VisitorMut for Remove {
        fn visit_table_ref(&mut self, table: &mut roastedduck_ast::TableRef) {
            if let roastedduck_ast::TableRefKind::TableFunction(function) = &table.kind
                && let roastedduck_ast::ParsedExpressionKind::Function(function) = &function.function.kind
                && (function.function_name.eq_ignore_ascii_case(b"read_csv_auto")
                    || function.function_name.eq_ignore_ascii_case(b"parquet_scan")
                    || function.function_name.eq_ignore_ascii_case(b"read_json_auto")
                    || function.function_name.eq_ignore_ascii_case(b"read_json"))
            {
                table.external_dependency = roastedduck_ast::ExternalDependency::default();
            }
            roastedduck_ast::visit::walk_table_ref(self, table);
        }
    }
    Remove.visit_query_node(&mut source.node);
    source
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub(super) fn read_parquet(
    owner: Bound<'_, PyConnection>,
    name: &Bound<'_, PyAny>,
    binary_as_string: bool,
    file_row_number: bool,
    filename: bool,
    hive_partitioning: bool,
    union_by_name: bool,
    compression: Option<&Bound<'_, PyAny>>,
) -> PyResult<crate::relation::PyRelation> {
    let py = owner.py();
    let state = Arc::clone(&owner.get().state);
    let (paths, owners) = paths(py, &state, name)?;
    let input = Value::list(
        LogicalType::VARCHAR,
        paths
            .iter()
            .map(|p| Value::try_varchar(p))
            .collect::<roastedduck_common::Result<_>>()
            .map_err(|e| error::native(py, &e))?,
    );
    let mut children = vec![roastedduck_parser::build::constant(input)];
    for (name, value) in [
        ("binary_as_string", binary_as_string),
        ("file_row_number", file_row_number),
        ("filename", filename),
        ("hive_partitioning", hive_partitioning),
        ("union_by_name", union_by_name),
    ] {
        let mut value = roastedduck_parser::build::constant(Value::boolean(value));
        value.alias = name.into();
        children.push(value);
    }
    if let Some(compression) = compression.filter(|v| !v.is_none()) {
        if !compression.is_instance_of::<PyString>() {
            return Err(invalid(py, "from_parquet only accepts 'compression' as a string"));
        }
        let mut value = roastedduck_parser::build::constant(Value::varchar(compression.extract::<String>()?));
        value.alias = "compression".into();
        children.push(value);
    }
    let alias = format!(
        "parquet_{:.16}",
        py.import("uuid")?
            .call_method0("uuid4")?
            .getattr("hex")?
            .extract::<String>()?
    );
    let mut table = roastedduck_parser::build::table_ref(roastedduck_ast::TableRefKind::TableFunction(
        roastedduck_ast::TableFunctionRef {
            function: Box::new(roastedduck_parser::build::function("parquet_scan", children)),
            column_name_alias: Vec::new(),
            with_ordinality: roastedduck_ast::OrdinalityType::WithoutOrdinality,
        },
    ));
    table.alias = alias.into();
    table.external_dependency = roastedduck_ast::ExternalDependency::new(Arc::new(owners));
    crate::relation::PyRelation::from_parquet_table(owner, table)
}
fn field_ids(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    let py = object.py();
    let dictionary = object
        .cast::<PyDict>()
        .map_err(|_| invalid(py, "NestedDictToStruct only accepts a dictionary as input"))?;
    let mut fields = Vec::new();
    let mut values = Vec::new();
    for (key, value) in dictionary.iter() {
        if !key.is_instance_of::<PyString>() {
            return Err(invalid(
                py,
                "NestedDictToStruct only accepts a dictionary with string keys",
            ));
        }
        let value = if value.is_instance_of::<PyInt>() {
            Value::integer(value.extract()?)
        } else if value.is_instance_of::<PyDict>() {
            field_ids(&value)?
        } else {
            return Err(invalid(
                py,
                "NestedDictToStruct only accepts a dictionary with integer values or nested dictionaries",
            ));
        };
        fields.push((key.extract::<String>()?.into(), value.logical_type().clone()));
        values.push(value);
    }
    Ok(Value::from_children(LogicalType::struct_type(fields), values))
}
pub(super) fn parquet_write_options(
    py: Python<'_>,
    options: &[(&str, Option<&Bound<'_, PyAny>>)],
) -> PyResult<CaseInsensitiveMap<Vec<Value>, ByteString>> {
    let mut result = CaseInsensitiveMap::default();
    for &(name, value) in options {
        let Some(value) = value.filter(|v| !v.is_none()) else {
            continue;
        };
        let key = if name == "overwrite" {
            "overwrite_or_ignore"
        } else {
            name
        };
        let values = match name {
            "compression" | "filename_pattern" => {
                if !value.is_instance_of::<PyString>() {
                    return Err(invalid(
                        py,
                        format!("to_parquet only accepts '{name}' as a string"),
                    ));
                }
                vec![Value::varchar(value.extract::<String>()?)]
            }
            "field_ids" => {
                if value.is_instance_of::<PyDict>() {
                    vec![field_ids(value)?]
                } else if value.is_instance_of::<PyString>() {
                    vec![Value::varchar(value.extract::<String>()?)]
                } else {
                    return Err(invalid(
                        py,
                        "to_parquet only accepts 'field_ids' as a dictionary or 'auto'",
                    ));
                }
            }
            "row_group_size_bytes" | "file_size_bytes" => {
                if value.is_instance_of::<PyInt>() {
                    vec![Value::bigint(value.extract()?)]
                } else if value.is_instance_of::<PyString>() {
                    vec![Value::varchar(value.extract::<String>()?)]
                } else {
                    return Err(invalid(
                        py,
                        format!(
                            "to_parquet only accepts '{name}' as an integer or {}string",
                            if name == "row_group_size_bytes" {
                                "'auto' "
                            } else {
                                ""
                            }
                        ),
                    ));
                }
            }
            "row_group_size" => {
                if !value.is_instance_of::<PyInt>() {
                    return Err(invalid(
                        py,
                        "to_parquet only accepts 'row_group_size' as an integer",
                    ));
                }
                vec![Value::bigint(value.extract()?)]
            }
            "partition_by" => strings(
                value,
                true,
                "to_parquet only accepts 'partition_by' as a list of strings",
            )?,
            _ => {
                if !value.is_instance_of::<PyBool>() {
                    return Err(invalid(
                        py,
                        format!("to_parquet only accepts '{name}' as a boolean"),
                    ));
                }
                vec![Value::boolean(value.extract()?)]
            }
        };
        result.insert(key, values);
    }
    Ok(result)
}

pub(super) fn parquet_call(
    owner: Bound<'_, PyConnection>,
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
    name: &str,
) -> PyResult<crate::relation::PyRelation> {
    let py = owner.py();
    let signature = "(self: _duckdb.DuckDBPyConnection, path_or_buffer: object, binary_as_string: bool = False, *, file_row_number: bool = False, filename: bool = False, hive_partitioning: bool = False, union_by_name: bool = False, compression: object = None) -> _duckdb.DuckDBPyRelation";
    let bad = || crate::error::arguments(py, name, signature, Some(owner.as_any()), args, kwargs);
    let names = [
        "path_or_buffer",
        "binary_as_string",
        "file_row_number",
        "filename",
        "hive_partitioning",
        "union_by_name",
        "compression",
    ];
    if args.len() > 2 {
        return Err(bad()?);
    }
    if let Some(kwargs) = kwargs {
        for (key, _) in kwargs.iter() {
            let key = key.extract::<String>()?;
            if !names.contains(&key.as_str()) {
                return Err(bad()?);
            }
        }
    }
    let mut parameters = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let keyword = kwargs.map(|kw| kw.get_item(name)).transpose()?.flatten();
        let positional = if index < args.len() {
            Some(args.get_item(index)?)
        } else {
            None
        };
        if keyword.is_some() && positional.is_some() {
            return Err(bad()?);
        }
        parameters.push(keyword.or(positional));
    }
    let Some(path) = parameters[0].as_ref() else {
        return Err(bad()?);
    };
    let mut flags = [false; 5];
    for (flag, parameter) in flags.iter_mut().zip(&parameters[1..6]) {
        if let Some(parameter) = parameter {
            if parameter.is_none() {
                continue;
            }
            // SAFETY: the live Python object is protected by the GIL. This is the
            // same numeric acceptance test used by the frozen pybind bool caster.
            if unsafe { pyo3::ffi::PyNumber_Check(parameter.as_ptr()) } == 0 {
                return Err(bad()?);
            }
            *flag = parameter.is_truthy()?;
        }
    }
    read_parquet(
        owner,
        path,
        flags[0],
        flags[1],
        flags[2],
        flags[3],
        flags[4],
        parameters[6].as_ref(),
    )
}
