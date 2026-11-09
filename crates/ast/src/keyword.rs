//! SQL keywords and identifier quoting (DuckDB's `KeywordHelper`, `SQLIdentifier`,
//! `SQLString`).
//!
//! The keyword table is generated from DuckDB's grammar keyword list
//! (`third_party/libpg_query/include/parser/kwlist.hpp`): every
//! `PG_KEYWORD("name", TOKEN, CATEGORY)` line becomes `("name", KeywordCategory::Category)`
//! (`UNRESERVED_KEYWORD` → `Unreserved`, `COL_NAME_KEYWORD` → `ColName`,
//! `TYPE_FUNC_NAME_KEYWORD` → `TypeFunc`, `RESERVED_KEYWORD` → `Reserved`).

/// The grammar category of a keyword (DuckDB's `KeywordCategory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeywordCategory {
    Reserved,
    Unreserved,
    TypeFunc,
    ColName,
    /// Not a keyword.
    None,
}

/// Every keyword of the grammar with its category, sorted by name. Generated (see the
/// module documentation).
#[rustfmt::skip]
const KEYWORDS: &[(&str, KeywordCategory)] = &[
    ("abort", KeywordCategory::Unreserved),
    ("absolute", KeywordCategory::Unreserved),
    ("access", KeywordCategory::Unreserved),
    ("action", KeywordCategory::Unreserved),
    ("add", KeywordCategory::Unreserved),
    ("admin", KeywordCategory::Unreserved),
    ("after", KeywordCategory::Unreserved),
    ("aggregate", KeywordCategory::Unreserved),
    ("all", KeywordCategory::Reserved),
    ("also", KeywordCategory::Unreserved),
    ("alter", KeywordCategory::Unreserved),
    ("always", KeywordCategory::Unreserved),
    ("analyse", KeywordCategory::Reserved),
    ("analyze", KeywordCategory::Reserved),
    ("and", KeywordCategory::Reserved),
    ("anti", KeywordCategory::TypeFunc),
    ("any", KeywordCategory::Reserved),
    ("array", KeywordCategory::Reserved),
    ("as", KeywordCategory::Reserved),
    ("asc", KeywordCategory::Reserved),
    ("asof", KeywordCategory::TypeFunc),
    ("assertion", KeywordCategory::Unreserved),
    ("assignment", KeywordCategory::Unreserved),
    ("asymmetric", KeywordCategory::Reserved),
    ("at", KeywordCategory::TypeFunc),
    ("attach", KeywordCategory::Unreserved),
    ("attribute", KeywordCategory::Unreserved),
    ("authorization", KeywordCategory::TypeFunc),
    ("backward", KeywordCategory::Unreserved),
    ("before", KeywordCategory::Unreserved),
    ("begin", KeywordCategory::Unreserved),
    ("between", KeywordCategory::ColName),
    ("bigint", KeywordCategory::ColName),
    ("binary", KeywordCategory::TypeFunc),
    ("bit", KeywordCategory::ColName),
    ("boolean", KeywordCategory::ColName),
    ("both", KeywordCategory::Reserved),
    ("by", KeywordCategory::TypeFunc),
    ("cache", KeywordCategory::Unreserved),
    ("call", KeywordCategory::Unreserved),
    ("called", KeywordCategory::Unreserved),
    ("cascade", KeywordCategory::Unreserved),
    ("cascaded", KeywordCategory::Unreserved),
    ("case", KeywordCategory::Reserved),
    ("cast", KeywordCategory::Reserved),
    ("catalog", KeywordCategory::Unreserved),
    ("centuries", KeywordCategory::Unreserved),
    ("century", KeywordCategory::Unreserved),
    ("chain", KeywordCategory::Unreserved),
    ("char", KeywordCategory::ColName),
    ("character", KeywordCategory::ColName),
    ("characteristics", KeywordCategory::Unreserved),
    ("check", KeywordCategory::Reserved),
    ("checkpoint", KeywordCategory::Unreserved),
    ("class", KeywordCategory::Unreserved),
    ("close", KeywordCategory::Unreserved),
    ("cluster", KeywordCategory::Unreserved),
    ("coalesce", KeywordCategory::ColName),
    ("collate", KeywordCategory::Reserved),
    ("collation", KeywordCategory::TypeFunc),
    ("column", KeywordCategory::Reserved),
    ("columns", KeywordCategory::TypeFunc),
    ("comment", KeywordCategory::Unreserved),
    ("comments", KeywordCategory::Unreserved),
    ("commit", KeywordCategory::Unreserved),
    ("committed", KeywordCategory::Unreserved),
    ("compression", KeywordCategory::Unreserved),
    ("concurrently", KeywordCategory::TypeFunc),
    ("configuration", KeywordCategory::Unreserved),
    ("conflict", KeywordCategory::Unreserved),
    ("connection", KeywordCategory::Unreserved),
    ("constraint", KeywordCategory::Reserved),
    ("constraints", KeywordCategory::Unreserved),
    ("content", KeywordCategory::Unreserved),
    ("continue", KeywordCategory::Unreserved),
    ("conversion", KeywordCategory::Unreserved),
    ("copy", KeywordCategory::Unreserved),
    ("cost", KeywordCategory::Unreserved),
    ("create", KeywordCategory::Reserved),
    ("cross", KeywordCategory::TypeFunc),
    ("csv", KeywordCategory::Unreserved),
    ("cube", KeywordCategory::Unreserved),
    ("current", KeywordCategory::Unreserved),
    ("cursor", KeywordCategory::Unreserved),
    ("cycle", KeywordCategory::Unreserved),
    ("data", KeywordCategory::Unreserved),
    ("database", KeywordCategory::Unreserved),
    ("day", KeywordCategory::Unreserved),
    ("days", KeywordCategory::Unreserved),
    ("deallocate", KeywordCategory::Unreserved),
    ("dec", KeywordCategory::ColName),
    ("decade", KeywordCategory::Unreserved),
    ("decades", KeywordCategory::Unreserved),
    ("decimal", KeywordCategory::ColName),
    ("declare", KeywordCategory::Unreserved),
    ("default", KeywordCategory::Reserved),
    ("defaults", KeywordCategory::Unreserved),
    ("deferrable", KeywordCategory::Reserved),
    ("deferred", KeywordCategory::Unreserved),
    ("definer", KeywordCategory::Unreserved),
    ("delete", KeywordCategory::Unreserved),
    ("delimiter", KeywordCategory::Unreserved),
    ("delimiters", KeywordCategory::Unreserved),
    ("depends", KeywordCategory::Unreserved),
    ("desc", KeywordCategory::Reserved),
    ("describe", KeywordCategory::Reserved),
    ("detach", KeywordCategory::Unreserved),
    ("dictionary", KeywordCategory::Unreserved),
    ("disable", KeywordCategory::Unreserved),
    ("discard", KeywordCategory::Unreserved),
    ("distinct", KeywordCategory::Reserved),
    ("do", KeywordCategory::Reserved),
    ("document", KeywordCategory::Unreserved),
    ("domain", KeywordCategory::Unreserved),
    ("double", KeywordCategory::Unreserved),
    ("drop", KeywordCategory::Unreserved),
    ("each", KeywordCategory::Unreserved),
    ("else", KeywordCategory::Reserved),
    ("enable", KeywordCategory::Unreserved),
    ("encoding", KeywordCategory::Unreserved),
    ("encrypted", KeywordCategory::Unreserved),
    ("end", KeywordCategory::Reserved),
    ("enum", KeywordCategory::Unreserved),
    ("error", KeywordCategory::Unreserved),
    ("escape", KeywordCategory::Unreserved),
    ("event", KeywordCategory::Unreserved),
    ("except", KeywordCategory::Reserved),
    ("exclude", KeywordCategory::Unreserved),
    ("excluding", KeywordCategory::Unreserved),
    ("exclusive", KeywordCategory::Unreserved),
    ("execute", KeywordCategory::Unreserved),
    ("exists", KeywordCategory::ColName),
    ("explain", KeywordCategory::Unreserved),
    ("export", KeywordCategory::Unreserved),
    ("export_state", KeywordCategory::Unreserved),
    ("extension", KeywordCategory::Unreserved),
    ("extensions", KeywordCategory::Unreserved),
    ("external", KeywordCategory::Unreserved),
    ("extract", KeywordCategory::ColName),
    ("false", KeywordCategory::Reserved),
    ("family", KeywordCategory::Unreserved),
    ("fetch", KeywordCategory::Reserved),
    ("filter", KeywordCategory::Unreserved),
    ("first", KeywordCategory::Unreserved),
    ("float", KeywordCategory::ColName),
    ("following", KeywordCategory::Unreserved),
    ("for", KeywordCategory::Reserved),
    ("force", KeywordCategory::Unreserved),
    ("foreign", KeywordCategory::Reserved),
    ("forward", KeywordCategory::Unreserved),
    ("freeze", KeywordCategory::TypeFunc),
    ("from", KeywordCategory::Reserved),
    ("full", KeywordCategory::TypeFunc),
    ("function", KeywordCategory::Unreserved),
    ("functions", KeywordCategory::Unreserved),
    ("generated", KeywordCategory::TypeFunc),
    ("glob", KeywordCategory::TypeFunc),
    ("global", KeywordCategory::Unreserved),
    ("grant", KeywordCategory::Unreserved),
    ("granted", KeywordCategory::Unreserved),
    ("group", KeywordCategory::Reserved),
    ("grouping", KeywordCategory::ColName),
    ("grouping_id", KeywordCategory::ColName),
    ("groups", KeywordCategory::Unreserved),
    ("handler", KeywordCategory::Unreserved),
    ("having", KeywordCategory::Reserved),
    ("header", KeywordCategory::Unreserved),
    ("hold", KeywordCategory::Unreserved),
    ("hour", KeywordCategory::Unreserved),
    ("hours", KeywordCategory::Unreserved),
    ("identity", KeywordCategory::Unreserved),
    ("if", KeywordCategory::Unreserved),
    ("ignore", KeywordCategory::Unreserved),
    ("ilike", KeywordCategory::TypeFunc),
    ("immediate", KeywordCategory::Unreserved),
    ("immutable", KeywordCategory::Unreserved),
    ("implicit", KeywordCategory::Unreserved),
    ("import", KeywordCategory::Unreserved),
    ("in", KeywordCategory::Reserved),
    ("include", KeywordCategory::Unreserved),
    ("including", KeywordCategory::Unreserved),
    ("increment", KeywordCategory::Unreserved),
    ("index", KeywordCategory::Unreserved),
    ("indexes", KeywordCategory::Unreserved),
    ("inherit", KeywordCategory::Unreserved),
    ("inherits", KeywordCategory::Unreserved),
    ("initially", KeywordCategory::Reserved),
    ("inline", KeywordCategory::Unreserved),
    ("inner", KeywordCategory::TypeFunc),
    ("inout", KeywordCategory::ColName),
    ("input", KeywordCategory::Unreserved),
    ("insensitive", KeywordCategory::Unreserved),
    ("insert", KeywordCategory::Unreserved),
    ("install", KeywordCategory::Unreserved),
    ("instead", KeywordCategory::Unreserved),
    ("int", KeywordCategory::ColName),
    ("integer", KeywordCategory::ColName),
    ("intersect", KeywordCategory::Reserved),
    ("interval", KeywordCategory::ColName),
    ("into", KeywordCategory::Reserved),
    ("invoker", KeywordCategory::Unreserved),
    ("is", KeywordCategory::TypeFunc),
    ("isnull", KeywordCategory::TypeFunc),
    ("isolation", KeywordCategory::Unreserved),
    ("join", KeywordCategory::TypeFunc),
    ("json", KeywordCategory::Unreserved),
    ("key", KeywordCategory::Unreserved),
    ("label", KeywordCategory::Unreserved),
    ("lambda", KeywordCategory::Reserved),
    ("language", KeywordCategory::Unreserved),
    ("large", KeywordCategory::Unreserved),
    ("last", KeywordCategory::Unreserved),
    ("lateral", KeywordCategory::Reserved),
    ("leading", KeywordCategory::Reserved),
    ("leakproof", KeywordCategory::Unreserved),
    ("left", KeywordCategory::TypeFunc),
    ("level", KeywordCategory::Unreserved),
    ("like", KeywordCategory::TypeFunc),
    ("limit", KeywordCategory::Reserved),
    ("listen", KeywordCategory::Unreserved),
    ("load", KeywordCategory::Unreserved),
    ("local", KeywordCategory::Unreserved),
    ("location", KeywordCategory::Unreserved),
    ("lock", KeywordCategory::Unreserved),
    ("locked", KeywordCategory::Unreserved),
    ("logged", KeywordCategory::Unreserved),
    ("macro", KeywordCategory::Unreserved),
    ("map", KeywordCategory::TypeFunc),
    ("mapping", KeywordCategory::Unreserved),
    ("match", KeywordCategory::Unreserved),
    ("matched", KeywordCategory::Unreserved),
    ("materialized", KeywordCategory::Unreserved),
    ("maxvalue", KeywordCategory::Unreserved),
    ("merge", KeywordCategory::Unreserved),
    ("method", KeywordCategory::Unreserved),
    ("microsecond", KeywordCategory::Unreserved),
    ("microseconds", KeywordCategory::Unreserved),
    ("millennia", KeywordCategory::Unreserved),
    ("millennium", KeywordCategory::Unreserved),
    ("millisecond", KeywordCategory::Unreserved),
    ("milliseconds", KeywordCategory::Unreserved),
    ("minute", KeywordCategory::Unreserved),
    ("minutes", KeywordCategory::Unreserved),
    ("minvalue", KeywordCategory::Unreserved),
    ("mode", KeywordCategory::Unreserved),
    ("month", KeywordCategory::Unreserved),
    ("months", KeywordCategory::Unreserved),
    ("move", KeywordCategory::Unreserved),
    ("name", KeywordCategory::Unreserved),
    ("names", KeywordCategory::Unreserved),
    ("national", KeywordCategory::ColName),
    ("natural", KeywordCategory::TypeFunc),
    ("nchar", KeywordCategory::ColName),
    ("new", KeywordCategory::Unreserved),
    ("next", KeywordCategory::Unreserved),
    ("no", KeywordCategory::Unreserved),
    ("none", KeywordCategory::ColName),
    ("not", KeywordCategory::Reserved),
    ("nothing", KeywordCategory::Unreserved),
    ("notify", KeywordCategory::Unreserved),
    ("notnull", KeywordCategory::TypeFunc),
    ("nowait", KeywordCategory::Unreserved),
    ("null", KeywordCategory::Reserved),
    ("nullif", KeywordCategory::ColName),
    ("nulls", KeywordCategory::Unreserved),
    ("numeric", KeywordCategory::ColName),
    ("object", KeywordCategory::Unreserved),
    ("of", KeywordCategory::Unreserved),
    ("off", KeywordCategory::Unreserved),
    ("offset", KeywordCategory::Reserved),
    ("oids", KeywordCategory::Unreserved),
    ("old", KeywordCategory::Unreserved),
    ("on", KeywordCategory::Reserved),
    ("only", KeywordCategory::Reserved),
    ("operator", KeywordCategory::Unreserved),
    ("option", KeywordCategory::Unreserved),
    ("options", KeywordCategory::Unreserved),
    ("or", KeywordCategory::Reserved),
    ("order", KeywordCategory::Reserved),
    ("ordinality", KeywordCategory::Unreserved),
    ("others", KeywordCategory::Unreserved),
    ("out", KeywordCategory::ColName),
    ("outer", KeywordCategory::TypeFunc),
    ("over", KeywordCategory::Unreserved),
    ("overlaps", KeywordCategory::TypeFunc),
    ("overlay", KeywordCategory::ColName),
    ("overriding", KeywordCategory::Unreserved),
    ("owned", KeywordCategory::Unreserved),
    ("owner", KeywordCategory::Unreserved),
    ("parallel", KeywordCategory::Unreserved),
    ("parser", KeywordCategory::Unreserved),
    ("partial", KeywordCategory::Unreserved),
    ("partition", KeywordCategory::Unreserved),
    ("partitioned", KeywordCategory::Unreserved),
    ("passing", KeywordCategory::Unreserved),
    ("password", KeywordCategory::Unreserved),
    ("percent", KeywordCategory::Unreserved),
    ("persistent", KeywordCategory::Unreserved),
    ("pivot", KeywordCategory::Reserved),
    ("pivot_longer", KeywordCategory::Reserved),
    ("pivot_wider", KeywordCategory::Reserved),
    ("placing", KeywordCategory::Reserved),
    ("plans", KeywordCategory::Unreserved),
    ("policy", KeywordCategory::Unreserved),
    ("position", KeywordCategory::ColName),
    ("positional", KeywordCategory::TypeFunc),
    ("pragma", KeywordCategory::Unreserved),
    ("preceding", KeywordCategory::Unreserved),
    ("precision", KeywordCategory::ColName),
    ("prepare", KeywordCategory::Unreserved),
    ("prepared", KeywordCategory::Unreserved),
    ("preserve", KeywordCategory::Unreserved),
    ("primary", KeywordCategory::Reserved),
    ("prior", KeywordCategory::Unreserved),
    ("privileges", KeywordCategory::Unreserved),
    ("procedural", KeywordCategory::Unreserved),
    ("procedure", KeywordCategory::Unreserved),
    ("program", KeywordCategory::Unreserved),
    ("publication", KeywordCategory::Unreserved),
    ("qualify", KeywordCategory::Reserved),
    ("quarter", KeywordCategory::Unreserved),
    ("quarters", KeywordCategory::Unreserved),
    ("quote", KeywordCategory::Unreserved),
    ("range", KeywordCategory::Unreserved),
    ("read", KeywordCategory::Unreserved),
    ("real", KeywordCategory::ColName),
    ("reassign", KeywordCategory::Unreserved),
    ("recheck", KeywordCategory::Unreserved),
    ("recursive", KeywordCategory::Unreserved),
    ("ref", KeywordCategory::Unreserved),
    ("references", KeywordCategory::Reserved),
    ("referencing", KeywordCategory::Unreserved),
    ("refresh", KeywordCategory::Unreserved),
    ("reindex", KeywordCategory::Unreserved),
    ("relative", KeywordCategory::Unreserved),
    ("release", KeywordCategory::Unreserved),
    ("rename", KeywordCategory::Unreserved),
    ("repeatable", KeywordCategory::Unreserved),
    ("replace", KeywordCategory::Unreserved),
    ("replica", KeywordCategory::Unreserved),
    ("reset", KeywordCategory::Unreserved),
    ("respect", KeywordCategory::Unreserved),
    ("restart", KeywordCategory::Unreserved),
    ("restrict", KeywordCategory::Unreserved),
    ("returning", KeywordCategory::Reserved),
    ("returns", KeywordCategory::Unreserved),
    ("revoke", KeywordCategory::Unreserved),
    ("right", KeywordCategory::TypeFunc),
    ("role", KeywordCategory::Unreserved),
    ("rollback", KeywordCategory::Unreserved),
    ("rollup", KeywordCategory::Unreserved),
    ("row", KeywordCategory::ColName),
    ("rows", KeywordCategory::Unreserved),
    ("rule", KeywordCategory::Unreserved),
    ("sample", KeywordCategory::Unreserved),
    ("savepoint", KeywordCategory::Unreserved),
    ("schema", KeywordCategory::Unreserved),
    ("schemas", KeywordCategory::Unreserved),
    ("scope", KeywordCategory::Unreserved),
    ("scroll", KeywordCategory::Unreserved),
    ("search", KeywordCategory::Unreserved),
    ("second", KeywordCategory::Unreserved),
    ("seconds", KeywordCategory::Unreserved),
    ("secret", KeywordCategory::Unreserved),
    ("security", KeywordCategory::Unreserved),
    ("select", KeywordCategory::Reserved),
    ("semi", KeywordCategory::TypeFunc),
    ("sequence", KeywordCategory::Unreserved),
    ("sequences", KeywordCategory::Unreserved),
    ("serializable", KeywordCategory::Unreserved),
    ("server", KeywordCategory::Unreserved),
    ("session", KeywordCategory::Unreserved),
    ("set", KeywordCategory::Unreserved),
    ("setof", KeywordCategory::ColName),
    ("sets", KeywordCategory::Unreserved),
    ("share", KeywordCategory::Unreserved),
    ("show", KeywordCategory::Reserved),
    ("similar", KeywordCategory::TypeFunc),
    ("simple", KeywordCategory::Unreserved),
    ("skip", KeywordCategory::Unreserved),
    ("smallint", KeywordCategory::ColName),
    ("snapshot", KeywordCategory::Unreserved),
    ("some", KeywordCategory::Reserved),
    ("sorted", KeywordCategory::Unreserved),
    ("source", KeywordCategory::Unreserved),
    ("sql", KeywordCategory::Unreserved),
    ("stable", KeywordCategory::Unreserved),
    ("standalone", KeywordCategory::Unreserved),
    ("start", KeywordCategory::Unreserved),
    ("statement", KeywordCategory::Unreserved),
    ("statistics", KeywordCategory::Unreserved),
    ("stdin", KeywordCategory::Unreserved),
    ("stdout", KeywordCategory::Unreserved),
    ("storage", KeywordCategory::Unreserved),
    ("stored", KeywordCategory::Unreserved),
    ("strict", KeywordCategory::Unreserved),
    ("strip", KeywordCategory::Unreserved),
    ("struct", KeywordCategory::TypeFunc),
    ("subscription", KeywordCategory::Unreserved),
    ("substring", KeywordCategory::ColName),
    ("summarize", KeywordCategory::Reserved),
    ("symmetric", KeywordCategory::Reserved),
    ("sysid", KeywordCategory::Unreserved),
    ("system", KeywordCategory::Unreserved),
    ("table", KeywordCategory::Reserved),
    ("tables", KeywordCategory::Unreserved),
    ("tablesample", KeywordCategory::TypeFunc),
    ("tablespace", KeywordCategory::Unreserved),
    ("target", KeywordCategory::Unreserved),
    ("temp", KeywordCategory::Unreserved),
    ("template", KeywordCategory::Unreserved),
    ("temporary", KeywordCategory::Unreserved),
    ("text", KeywordCategory::Unreserved),
    ("then", KeywordCategory::Reserved),
    ("ties", KeywordCategory::Unreserved),
    ("time", KeywordCategory::ColName),
    ("timestamp", KeywordCategory::ColName),
    ("to", KeywordCategory::Reserved),
    ("trailing", KeywordCategory::Reserved),
    ("transaction", KeywordCategory::Unreserved),
    ("transform", KeywordCategory::Unreserved),
    ("treat", KeywordCategory::ColName),
    ("trigger", KeywordCategory::Unreserved),
    ("trim", KeywordCategory::ColName),
    ("true", KeywordCategory::Reserved),
    ("truncate", KeywordCategory::Unreserved),
    ("trusted", KeywordCategory::Unreserved),
    ("try_cast", KeywordCategory::TypeFunc),
    ("type", KeywordCategory::Unreserved),
    ("types", KeywordCategory::Unreserved),
    ("unbounded", KeywordCategory::Unreserved),
    ("uncommitted", KeywordCategory::Unreserved),
    ("unencrypted", KeywordCategory::Unreserved),
    ("union", KeywordCategory::Reserved),
    ("unique", KeywordCategory::Reserved),
    ("unknown", KeywordCategory::Unreserved),
    ("unlisten", KeywordCategory::Unreserved),
    ("unlogged", KeywordCategory::Unreserved),
    ("unpack", KeywordCategory::TypeFunc),
    ("unpivot", KeywordCategory::Reserved),
    ("until", KeywordCategory::Unreserved),
    ("update", KeywordCategory::Unreserved),
    ("use", KeywordCategory::Unreserved),
    ("user", KeywordCategory::Unreserved),
    ("using", KeywordCategory::Reserved),
    ("vacuum", KeywordCategory::Unreserved),
    ("valid", KeywordCategory::Unreserved),
    ("validate", KeywordCategory::Unreserved),
    ("validator", KeywordCategory::Unreserved),
    ("value", KeywordCategory::Unreserved),
    ("values", KeywordCategory::ColName),
    ("varchar", KeywordCategory::ColName),
    ("variable", KeywordCategory::Unreserved),
    ("variadic", KeywordCategory::Reserved),
    ("varying", KeywordCategory::Unreserved),
    ("verbose", KeywordCategory::TypeFunc),
    ("version", KeywordCategory::Unreserved),
    ("view", KeywordCategory::Unreserved),
    ("views", KeywordCategory::Unreserved),
    ("virtual", KeywordCategory::Unreserved),
    ("volatile", KeywordCategory::Unreserved),
    ("week", KeywordCategory::Unreserved),
    ("weeks", KeywordCategory::Unreserved),
    ("when", KeywordCategory::Reserved),
    ("where", KeywordCategory::Reserved),
    ("whitespace", KeywordCategory::Unreserved),
    ("window", KeywordCategory::Reserved),
    ("with", KeywordCategory::Reserved),
    ("within", KeywordCategory::Unreserved),
    ("without", KeywordCategory::Unreserved),
    ("work", KeywordCategory::Unreserved),
    ("wrapper", KeywordCategory::Unreserved),
    ("write", KeywordCategory::Unreserved),
    ("xml", KeywordCategory::Unreserved),
    ("xmlattributes", KeywordCategory::ColName),
    ("xmlconcat", KeywordCategory::ColName),
    ("xmlelement", KeywordCategory::ColName),
    ("xmlexists", KeywordCategory::ColName),
    ("xmlforest", KeywordCategory::ColName),
    ("xmlnamespaces", KeywordCategory::ColName),
    ("xmlparse", KeywordCategory::ColName),
    ("xmlpi", KeywordCategory::ColName),
    ("xmlroot", KeywordCategory::ColName),
    ("xmlserialize", KeywordCategory::ColName),
    ("xmltable", KeywordCategory::ColName),
    ("year", KeywordCategory::Unreserved),
    ("years", KeywordCategory::Unreserved),
    ("yes", KeywordCategory::Unreserved),
    ("zone", KeywordCategory::Unreserved),
];

/// The category of `text` as a keyword, compared after ASCII lower-casing
/// (DuckDB's `Parser::IsKeyword`).
pub fn keyword_category(text: &str) -> KeywordCategory {
    let lower = text.to_ascii_lowercase();
    KEYWORDS
        .binary_search_by(|(name, _)| (*name).cmp(lower.as_str()))
        .map_or(KeywordCategory::None, |index| KEYWORDS[index].1)
}

/// Whether `text` is a keyword of any category other than `excluded`
/// (DuckDB's `KeywordHelper::IsKeyword`).
pub fn is_keyword(text: &str, excluded: KeywordCategory) -> bool {
    // C++ compares the categories directly, including KEYWORD_NONE. Thus a
    // non-keyword is quoted when a different category is excluded.
    keyword_category(text) != excluded
}

/// Whether `text` must be quoted to read back as the same identifier
/// (DuckDB's `KeywordHelper::RequiresQuotes`).
pub fn requires_quotes(text: &str, allow_caps: bool, excluded: KeywordCategory) -> bool {
    let plain = text.bytes().enumerate().all(|(i, byte)| {
        (i > 0 && byte.is_ascii_digit())
            || byte.is_ascii_lowercase()
            || (allow_caps && byte.is_ascii_uppercase())
            || byte == b'_'
    });
    !plain || is_keyword(text, excluded)
}

/// `text` wrapped in `quote`, with embedded quotes doubled
/// (DuckDB's `KeywordHelper::WriteQuoted`).
pub fn write_quoted(text: &str, quote: char) -> String {
    let mut result = String::with_capacity(text.len() + 2);
    result.push(quote);
    for c in text.chars() {
        if c == quote {
            result.push(quote);
        }
        result.push(c);
    }
    result.push(quote);
    result
}

/// `text`, quoted with `quote` only when needed
/// (DuckDB's `KeywordHelper::WriteOptionallyQuoted`).
pub fn write_optionally_quoted_with(
    text: &str,
    quote: char,
    allow_caps: bool,
    excluded: KeywordCategory,
) -> String {
    if requires_quotes(text, allow_caps, excluded) {
        write_quoted(text, quote)
    } else {
        text.to_owned()
    }
}

/// `text` double-quoted only when needed, capitals allowed (the defaults of DuckDB's
/// `KeywordHelper::WriteOptionallyQuoted`).
pub fn write_optionally_quoted(text: &str) -> String {
    write_optionally_quoted_with(text, '"', true, KeywordCategory::None)
}

/// The byte-preserving identifier form of `WriteOptionallyQuoted`.
pub fn write_optionally_quoted_bytes(text: &[u8]) -> roastedduck_common::ByteString {
    quoted_identifier_bytes(text, true)
}

/// The native byte form of `SQLIdentifier` (ASCII capitals are allowed).
pub fn sql_identifier_bytes(text: &[u8]) -> roastedduck_common::ByteString {
    quoted_identifier_bytes(text, true)
}

/// Native byte form of the configurable keyword-aware identifier writer.
pub fn write_optionally_quoted_with_bytes(
    text: &[u8],
    quote: u8,
    allow_caps: bool,
    excluded: KeywordCategory,
) -> roastedduck_common::ByteString {
    let plain = text.iter().enumerate().all(|(i, byte)| {
        (i > 0 && byte.is_ascii_digit())
            || byte.is_ascii_lowercase()
            || (allow_caps && byte.is_ascii_uppercase())
            || *byte == b'_'
    });
    let lower = text.to_ascii_lowercase();
    let category = KEYWORDS
        .binary_search_by(|(name, _)| name.as_bytes().cmp(&lower))
        .map_or(KeywordCategory::None, |index| KEYWORDS[index].1);
    if plain && category == excluded {
        return text.into();
    }
    let mut out = roastedduck_common::ByteString::new();
    out.push(quote);
    for &byte in text {
        if byte == quote {
            out.push(byte);
        }
        out.push(byte);
    }
    out.push(quote);
    out
}
fn quoted_identifier_bytes(text: &[u8], allow_caps: bool) -> roastedduck_common::ByteString {
    write_optionally_quoted_with_bytes(text, b'"', allow_caps, KeywordCategory::None)
}

/// An identifier as DuckDB's `SQLIdentifier` renders it: quoted when required,
/// with ASCII capitals allowed.
pub fn sql_identifier(text: &str) -> String {
    write_optionally_quoted_with(text, '"', true, KeywordCategory::None)
}

/// Native SQL string quoting, without a VARCHAR-value validation boundary.
pub fn sql_string_bytes(text: &[u8]) -> roastedduck_common::ByteString {
    let mut out = roastedduck_common::ByteString::from(b"'");
    for &byte in text {
        if byte == b'\'' {
            out.push(byte);
        }
        out.push(byte);
    }
    out.push(b'\'');
    out
}

/// A string literal as DuckDB's `SQLString` renders it.
pub fn sql_string(text: &str) -> String {
    write_quoted(text, '\'')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_keyword_helper() {
        assert!(KEYWORDS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(keyword_category("SELECT"), KeywordCategory::Reserved);
        assert_eq!(keyword_category("integer"), KeywordCategory::ColName);
        assert_eq!(write_optionally_quoted("MyCol"), "MyCol");
        assert_eq!(write_optionally_quoted("select"), "\"select\"");
        assert_eq!(write_optionally_quoted("a b"), "\"a b\"");
        // Frozen C++ SQLIdentifier allows capitals (native oracle: SELECT 1 AS MyCol).
        assert_eq!(sql_identifier("MyCol"), "MyCol");
        assert_eq!(
            write_optionally_quoted_with("MyCol", '"', false, KeywordCategory::None),
            "\"MyCol\""
        );
        assert_eq!(
            write_optionally_quoted_with("my_type", '"', true, KeywordCategory::ColName),
            "\"my_type\""
        );
        assert_eq!(
            write_optionally_quoted_with("integer", '"', true, KeywordCategory::ColName),
            "integer"
        );
        assert_eq!(sql_string("it's"), "'it''s'");
    }
}
