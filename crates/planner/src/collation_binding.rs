//! Collations (DuckDB's `planner/collation_binding.cpp`): expressions that must compare,
//! group and sort by a normalised form get wrapped in a function computing it.
//!
//! VARCHAR collations (`nocase`, `noaccent`, `nfc`) wrap the string in the collation's
//! function; TIME WITH TIME ZONE compares through `timetz_byte_comparable` and INTERVAL
//! through `normalized_interval`, so `1 month = 30 days` also holds for grouping, sorting
//! and joins.

use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo};

use crate::context::ClientContext;
use crate::expression::{Expression, ExpressionKind};
use crate::function_binder::FunctionBinder;

/// Which collations to apply (DuckDB's `CollationType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollationType {
    /// Every collation.
    AllCollations,
    /// Only collations that can be combined with others (for functions such as `like`).
    CombinableCollations,
}

/// A builtin collation (DuckDB's `CollateCatalogEntry`, registered by
/// `BuiltinFunctions::AddCollation`).
#[derive(Debug)]
struct Collation {
    name: &'static str,
    /// The scalar function computing the collated value.
    function: &'static str,
    /// Whether the collation can be combined with other collations.
    combinable: bool,
}

/// `BuiltinFunctions::Initialize`: `nocase`, `noaccent` and `nfc`.
const COLLATIONS: &[Collation] = &[
    Collation {
        name: "nocase",
        function: "lower",
        combinable: true,
    },
    Collation {
        name: "noaccent",
        function: "strip_accents",
        combinable: true,
    },
    Collation {
        name: "nfc",
        function: "nfc_normalize",
        combinable: false,
    },
];

/// The names of the builtin collations, which are entries of the system catalog's main
/// schema in DuckDB.
pub(crate) fn builtin_collation_names() -> impl Iterator<Item = &'static str> {
    COLLATIONS.iter().map(|collation| collation.name)
}

/// The collation of a VARCHAR type (`StringType::GetCollation`), empty without one.
fn type_collation(ty: &LogicalType) -> &str {
    match ty.info() {
        Some(TypeInfo::String(info)) => &info.collation,
        _ => "",
    }
}

/// A collation a collation string names: a builtin one, or one an extension registered in
/// the system catalog (`CollateCatalogEntry`).
#[derive(Clone, Debug)]
enum ResolvedCollation {
    Builtin(&'static Collation),
    Extension {
        name: String,
        entry: Box<rustdb_catalog::CollationEntry>,
    },
}

impl ResolvedCollation {
    fn name(&self) -> &str {
        match self {
            Self::Builtin(collation) => collation.name,
            Self::Extension { name, .. } => name,
        }
    }

    fn combinable(&self) -> bool {
        match self {
            Self::Builtin(collation) => collation.combinable,
            Self::Extension { entry, .. } => entry.combinable,
        }
    }
}

/// Wraps `source` in the single overload of the system function `name`.
fn wrap(context: &ClientContext, name: &str, source: Expression) -> Result<Expression> {
    let entry = context
        .get_entry("system", "main", CatalogType::ScalarFunction, name, true)?
        .ok_or_else(|| Error::internal(format!("collation function {name} is missing")))?;
    let EntryKind::ScalarFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("{name} is not a scalar function")));
    };
    let [function] = functions.functions.as_slice() else {
        return Err(Error::internal(format!(
            "{name} should only have a single overload"
        )));
    };
    FunctionBinder::new(context).bind_scalar_overload(function.clone(), vec![source], false)
}

/// `PushVarcharCollation`.
fn push_varchar_collation(
    context: &ClientContext,
    source: Expression,
    ty: &LogicalType,
    kind: CollationType,
) -> Result<(Expression, bool)> {
    if ty.id() != LogicalTypeId::Varchar {
        return Ok((source, false));
    }
    let collation = match type_collation(ty) {
        "" => context.config.default_collation.as_str(),
        collation => collation,
    }
    .to_lowercase();
    // `Catalog::GetEntry<CollateCatalogEntry>`: a missing collation fails with the
    // catalog's missing-entry error.
    let entries = resolve_collations(&collation, |argument| {
        let entry = context.get_entry("system", "main", CatalogType::Collation, argument, false)?;
        Ok(entry.and_then(|entry| match &entry.entry.kind {
            EntryKind::Collation(collation) => Some(collation.clone()),
            _ => None,
        }))
    })?;
    if entries.is_empty() {
        return Ok((source, false));
    }
    let mut source = source;
    for entry in entries {
        if !entry.combinable() && kind == CollationType::CombinableCollations {
            return Ok((source, false));
        }
        source = match entry {
            ResolvedCollation::Builtin(collation) => {
                // The collation's own function object (`CollateCatalogEntry::function`),
                // not the catalog's.
                let mut collated = wrap(context, collation.function, source)?;
                if let ExpressionKind::Function(function) = &mut collated.kind {
                    function.function.clear_catalog_entry();
                }
                collated
            }
            ResolvedCollation::Extension { entry, .. } => {
                FunctionBinder::new(context).bind_scalar_overload(entry.function, vec![source], false)?
            }
        };
    }
    Ok((source, true))
}

/// Checks that `collation` names collations that can be combined (the check
/// `ExpressionBinder::TestCollation` makes for `SET default_collation`); `extensions` are
/// the collations extensions registered, with whether they are combinable.
pub fn test_collation(collation: &str, extensions: &[(String, bool)]) -> Result<()> {
    resolve_collations(&collation.to_lowercase(), |argument| {
        Ok(extensions
            .iter()
            .find(|(name, _)| name == argument)
            .map(|(_, combinable)| rustdb_catalog::CollationEntry {
                function: rustdb_function::ScalarFunction::new(
                    "",
                    vec![],
                    LogicalType::VARCHAR,
                    |_, _, _| Ok(()),
                ),
                combinable: *combinable,
                not_required_for_equality: false,
            }))
    })
    .map(|_| ())
}

/// The collations a lowercase `a.b.c` collation applies, combinable ones first;
/// none for the binary collations. `lookup` finds the collations extensions registered.
fn resolve_collations(
    collation: &str,
    lookup: impl Fn(&str) -> Result<Option<rustdb_catalog::CollationEntry>>,
) -> Result<Vec<ResolvedCollation>> {
    if collation.is_empty() || matches!(collation, "binary" | "c" | "posix") {
        return Ok(Vec::new());
    }
    let mut entries: Vec<ResolvedCollation> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for argument in collation.split('.') {
        if seen.contains(&argument) {
            continue;
        }
        let entry = match COLLATIONS.iter().find(|entry| entry.name == argument) {
            Some(entry) => ResolvedCollation::Builtin(entry),
            None => match lookup(argument)? {
                Some(entry) => ResolvedCollation::Extension {
                    name: argument.to_owned(),
                    entry: Box::new(entry),
                },
                None => {
                    return Err(Error::catalog(format!(
                        "Collation with name {argument} does not exist!"
                    )));
                }
            },
        };
        if entry.combinable() {
            entries.insert(0, entry);
        } else {
            if let Some(last) = entries.last()
                && !last.combinable()
            {
                return Err(Error::binder(format!(
                    "Cannot combine collation types \"{}\" and \"{}\"",
                    last.name(),
                    entry.name()
                )));
            }
            entries.push(entry);
        }
        seen.push(argument);
    }
    Ok(entries)
}

/// Applies the collation `ty` calls for to `source` (DuckDB's
/// `ExpressionBinder::PushCollation`); the flag tells whether one was applied.
pub fn push_collation_with(
    context: &ClientContext,
    source: Expression,
    ty: &LogicalType,
    kind: CollationType,
) -> Result<(Expression, bool)> {
    match ty.id() {
        LogicalTypeId::Varchar => push_varchar_collation(context, source, ty, kind),
        LogicalTypeId::TimeTz => Ok((wrap(context, "timetz_byte_comparable", source)?, true)),
        LogicalTypeId::Interval => Ok((wrap(context, "normalized_interval", source)?, true)),
        LogicalTypeId::Variant => {
            // `PushVariantCollation` keeps the alias of the source.
            let alias = source.alias.clone();
            let mut wrapped = wrap(context, "variant_normalize", source)?;
            wrapped.alias = alias;
            Ok((wrapped, true))
        }
        _ => Ok((source, false)),
    }
}

/// [`push_collation_with`] for every collation, returning only the expression.
pub fn push_collation(context: &ClientContext, source: Expression, ty: &LogicalType) -> Result<Expression> {
    push_collation_with(context, source, ty, CollationType::AllCollations).map(|(expression, _)| expression)
}
