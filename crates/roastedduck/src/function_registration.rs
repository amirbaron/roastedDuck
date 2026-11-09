//! Built-in versus `core_functions` registration, following the frozen generated lists.

use std::collections::BTreeMap;

use roastedduck_catalog::Catalog;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::{AggregateFunctionSet, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{LogicalType, LogicalTypeId};

use crate::function_lists::{BUILTIN_FUNCTIONS, CORE_FUNCTIONS, EXTENSION_OVERLOADS, FunctionKind};

/// `BuiltinFunctions::RegisterExtensionOverloads`: real typed scalar entries whose bind
/// callback resolves their provider, rather than untyped missing-function hints.
pub(crate) fn register_extension_overloads(catalog: &Catalog) -> Result<()> {
    let mut current: Option<ScalarFunctionSet> = None;
    for &(name, extension, signature) in EXTENSION_OVERLOADS {
        if current
            .as_ref()
            .is_some_and(|set| set.name.as_bytes() != name.as_bytes())
            && let Some(set) = current.take()
        {
            catalog.register_scalar_functions(set)?;
        }
        let (arguments, result) = signature
            .split_once('>')
            .ok_or_else(|| Error::internal("Invalid extension overload signature"))?;
        let arguments = arguments
            .strip_prefix('[')
            .and_then(|arguments| arguments.strip_suffix(']'))
            .ok_or_else(|| Error::internal("Invalid extension overload argument list"))?;
        let arguments = arguments
            .split(',')
            .filter(|ty| !ty.is_empty())
            .map(extension_type)
            .collect::<Result<Vec<_>>>()?;
        let function = ScalarFunction::new(name, arguments, extension_type(result)?, |_, _, _| {
            Err(Error::internal("Extension placeholder executed without binding"))
        })
        .with_bind(move |function, input| {
            crate::database::bind_extension_overload(function, input, extension)
        });
        current
            .get_or_insert_with(|| ScalarFunctionSet::new(name))
            .add(function);
    }
    if let Some(set) = current {
        catalog.register_scalar_functions(set)?;
    }
    Ok(())
}

fn extension_type(name: &str) -> Result<LogicalType> {
    let name = name.trim_matches('\'');
    if let Some(child) = name.strip_suffix("[]") {
        return extension_type(child).map(LogicalType::list);
    }
    Ok(match name {
        "BIGINT" => LogicalType::BIGINT,
        "DATE" => LogicalType::DATE,
        "DOUBLE" => LogicalType::DOUBLE,
        "INTERVAL" => LogicalType::INTERVAL,
        "STRUCT()" => LogicalType::struct_type(Vec::new()),
        "TIME" => LogicalType::TIME,
        "TIMESTAMP" => LogicalType::TIMESTAMP,
        "TIMESTAMPTZ" => LogicalType::TIMESTAMP_TZ,
        "TIMETZ" => LogicalType::TIME_TZ,
        "TIME_NS" => LogicalType::TIME_NS,
        "VARCHAR" => LogicalType::VARCHAR,
        _ => {
            return Err(Error::internal(format!(
                "Unknown extension overload type: {name}"
            )));
        }
    })
}

/// The real system-catalog entries created by `BuiltinFunctions::Initialize`.
pub(crate) fn register_builtin_collations(catalog: &Catalog) -> Result<()> {
    use roastedduck_functions::scalar::string::{caseconvert, normalize};

    for (name, mut functions, combinable) in [
        ("nocase", caseconvert::lower_functions(), true),
        ("noaccent", normalize::strip_accents_functions(), true),
        ("nfc", normalize::nfc_normalize_functions(), false),
    ] {
        let function = functions
            .functions
            .pop()
            .ok_or_else(|| Error::internal(format!("Missing built-in collation function: {name}")))?;
        catalog.register_builtin_collation(
            name,
            roastedduck_catalog::CollationEntry {
                function,
                combinable,
                not_required_for_equality: false,
            },
        )?;
    }
    Ok(())
}

#[derive(Debug)]
enum Function {
    Scalar(ScalarFunctionSet),
    Aggregate(AggregateFunctionSet),
}

impl Function {
    fn register(self, catalog: &Catalog, extension: bool) -> Result<()> {
        match self {
            Self::Scalar(set) if extension => catalog.register_scalar_functions_merged(set),
            Self::Scalar(set) => catalog.register_scalar_functions(set),
            Self::Aggregate(set) => catalog.register_aggregate_functions(set),
        }
    }
}

#[derive(Debug)]
pub(crate) struct StartupFunctions {
    builtin: Vec<Function>,
    core: CoreFunctions,
}

/// Unregistered function descriptors, released after startup; no table/result cache.
#[derive(Debug)]
pub(crate) struct CoreFunctions(Vec<Function>);

impl CoreFunctions {
    pub(crate) fn register(self, catalog: &Catalog) -> Result<()> {
        for function in self.0 {
            function.register(catalog, true)?;
        }
        Ok(())
    }
}

impl StartupFunctions {
    pub(crate) fn new() -> Result<Self> {
        let mut scalars = BTreeMap::new();
        for set in roastedduck_functions::builtin_scalar_functions()
            .into_iter()
            .chain(crate::sequence::sequence_functions())
            .chain([crate::switch::switch_functions()])
            .chain(crate::system_functions::context_scalar_functions())
        {
            let name = set.name.clone();
            if scalars.insert(name.clone(), set).is_some() {
                return Err(Error::internal(roastedduck_common::byte_format!(
                    "Duplicate startup scalar function: {name}"
                )));
            }
        }
        let mut aggregates = BTreeMap::new();
        for set in roastedduck_functions::builtin_aggregate_functions()? {
            let name = ByteString::from(set.name.as_bytes());
            if aggregates.insert(name.clone(), set).is_some() {
                return Err(Error::internal(roastedduck_common::byte_format!(
                    "Duplicate startup aggregate function: {name}"
                )));
            }
        }

        // && has a built-in GEOMETRY overload and a core_functions list overload.
        // The public function factory merges them; retain their real registration origins.
        let shared = scalars
            .remove(b"&&".as_slice())
            .ok_or_else(|| Error::internal("Missing shared && startup function"))?;
        let (geometry, lists): (Vec<_>, Vec<_>) = shared.functions.into_iter().partition(|function| {
            function
                .arguments
                .iter()
                .any(|ty| ty.id() == LogicalTypeId::Geometry)
        });
        if geometry.is_empty() || lists.is_empty() {
            return Err(Error::internal("Missing GEOMETRY or list && overload"));
        }
        scalars.insert(
            shared.name.clone(),
            ScalarFunctionSet {
                name: shared.name.clone(),
                functions: geometry,
            },
        );
        let builtin = take_functions(BUILTIN_FUNCTIONS, &mut scalars, &mut aggregates)?;
        scalars.insert(
            shared.name.clone(),
            ScalarFunctionSet {
                name: shared.name,
                functions: lists,
            },
        );
        let core = CoreFunctions(take_functions(CORE_FUNCTIONS, &mut scalars, &mut aggregates)?);
        if !scalars.is_empty() || !aggregates.is_empty() {
            return Err(Error::internal(format!(
                "Unclassified startup functions: scalar={:?}, aggregate={:?}",
                scalars.keys().collect::<Vec<_>>(),
                aggregates.keys().collect::<Vec<_>>()
            )));
        }
        Ok(Self { builtin, core })
    }

    pub(crate) fn register_builtin(self, catalog: &Catalog) -> Result<CoreFunctions> {
        for function in self.builtin {
            function.register(catalog, false)?;
        }
        Ok(self.core)
    }
}

fn take_functions(
    definitions: &[(FunctionKind, &str)],
    scalars: &mut BTreeMap<ByteString, ScalarFunctionSet>,
    aggregates: &mut BTreeMap<ByteString, AggregateFunctionSet>,
) -> Result<Vec<Function>> {
    definitions
        .iter()
        .map(|(kind, name)| {
            match kind {
                FunctionKind::Scalar => scalars.remove(name.as_bytes()).map(Function::Scalar),
                FunctionKind::Aggregate => aggregates.remove(name.as_bytes()).map(Function::Aggregate),
            }
            .ok_or_else(|| Error::internal(format!("Missing startup function definition: {name}")))
        })
        .collect()
}
