//! SQL identifier and literal quoting (DuckDB's `KeywordHelper`, `SQLIdentifier`).

use std::borrow::Cow;

/// Every keyword of DuckDB's grammar (`kwlist.hpp`), sorted. A keyword of any category
/// forces quoting.
#[rustfmt::skip]
const KEYWORDS: &[&str] = &[
    "abort", "absolute", "access", "action", "add", "admin", "after", "aggregate", "all", "also",
    "alter", "always", "analyse", "analyze", "and", "anti", "any", "array", "as", "asc", "asof",
    "assertion", "assignment", "asymmetric", "at", "attach", "attribute", "authorization", "backward",
    "before", "begin", "between", "bigint", "binary", "bit", "boolean", "both", "by", "cache", "call",
    "called", "cascade", "cascaded", "case", "cast", "catalog", "centuries", "century", "chain",
    "char", "character", "characteristics", "check", "checkpoint", "class", "close", "cluster",
    "coalesce", "collate", "collation", "column", "columns", "comment", "comments", "commit",
    "committed", "compression", "concurrently", "configuration", "conflict", "connection",
    "constraint", "constraints", "content", "continue", "conversion", "copy", "cost", "create",
    "cross", "csv", "cube", "current", "cursor", "cycle", "data", "database", "day", "days",
    "deallocate", "dec", "decade", "decades", "decimal", "declare", "default", "defaults",
    "deferrable", "deferred", "definer", "delete", "delimiter", "delimiters", "depends", "desc",
    "describe", "detach", "dictionary", "disable", "discard", "distinct", "do", "document", "domain",
    "double", "drop", "each", "else", "enable", "encoding", "encrypted", "end", "enum", "error",
    "escape", "event", "except", "exclude", "excluding", "exclusive", "execute", "exists", "explain",
    "export", "export_state", "extension", "extensions", "external", "extract", "false", "family",
    "fetch", "filter", "first", "float", "following", "for", "force", "foreign", "forward", "freeze",
    "from", "full", "function", "functions", "generated", "glob", "global", "grant", "granted",
    "group", "grouping", "grouping_id", "groups", "handler", "having", "header", "hold", "hour",
    "hours", "identity", "if", "ignore", "ilike", "immediate", "immutable", "implicit", "import", "in",
    "include", "including", "increment", "index", "indexes", "inherit", "inherits", "initially",
    "inline", "inner", "inout", "input", "insensitive", "insert", "install", "instead", "int",
    "integer", "intersect", "interval", "into", "invoker", "is", "isnull", "isolation", "join", "json",
    "key", "label", "lambda", "language", "large", "last", "lateral", "leading", "leakproof", "left",
    "level", "like", "limit", "listen", "load", "local", "location", "lock", "locked", "logged",
    "macro", "map", "mapping", "match", "matched", "materialized", "maxvalue", "merge", "method",
    "microsecond", "microseconds", "millennia", "millennium", "millisecond", "milliseconds", "minute",
    "minutes", "minvalue", "mode", "month", "months", "move", "name", "names", "national", "natural",
    "nchar", "new", "next", "no", "none", "not", "nothing", "notify", "notnull", "nowait", "null",
    "nullif", "nulls", "numeric", "object", "of", "off", "offset", "oids", "old", "on", "only",
    "operator", "option", "options", "or", "order", "ordinality", "others", "out", "outer", "over",
    "overlaps", "overlay", "overriding", "owned", "owner", "parallel", "parser", "partial",
    "partition", "partitioned", "passing", "password", "percent", "persistent", "pivot",
    "pivot_longer", "pivot_wider", "placing", "plans", "policy", "position", "positional", "pragma",
    "preceding", "precision", "prepare", "prepared", "preserve", "primary", "prior", "privileges",
    "procedural", "procedure", "program", "publication", "qualify", "quarter", "quarters", "quote",
    "range", "read", "real", "reassign", "recheck", "recursive", "ref", "references", "referencing",
    "refresh", "reindex", "relative", "release", "rename", "repeatable", "replace", "replica", "reset",
    "respect", "restart", "restrict", "returning", "returns", "revoke", "right", "role", "rollback",
    "rollup", "row", "rows", "rule", "sample", "savepoint", "schema", "schemas", "scope", "scroll",
    "search", "second", "seconds", "secret", "security", "select", "semi", "sequence", "sequences",
    "serializable", "server", "session", "set", "setof", "sets", "share", "show", "similar", "simple",
    "skip", "smallint", "snapshot", "some", "sorted", "source", "sql", "stable", "standalone", "start",
    "statement", "statistics", "stdin", "stdout", "storage", "stored", "strict", "strip", "struct",
    "subscription", "substring", "summarize", "symmetric", "sysid", "system", "table", "tables",
    "tablesample", "tablespace", "target", "temp", "template", "temporary", "text", "then", "ties",
    "time", "timestamp", "to", "trailing", "transaction", "transform", "treat", "trigger", "trim",
    "true", "truncate", "trusted", "try_cast", "type", "types", "unbounded", "uncommitted",
    "unencrypted", "union", "unique", "unknown", "unlisten", "unlogged", "unpack", "unpivot", "until",
    "update", "use", "user", "using", "vacuum", "valid", "validate", "validator", "value", "values",
    "varchar", "variable", "variadic", "varying", "verbose", "version", "view", "views", "virtual",
    "volatile", "week", "weeks", "when", "where", "whitespace", "window", "with", "within", "without",
    "work", "wrapper", "write", "xml", "xmlattributes", "xmlconcat", "xmlelement", "xmlexists",
    "xmlforest", "xmlnamespaces", "xmlparse", "xmlpi", "xmlroot", "xmlserialize", "xmltable", "year",
    "years", "yes", "zone",
];

/// Whether `name` is a keyword of the SQL grammar, compared after ASCII lower-casing
/// (DuckDB's `Parser::IsKeyword`).
pub fn is_keyword(name: &str) -> bool {
    KEYWORDS
        .binary_search(&name.to_ascii_lowercase().as_str())
        .is_ok()
}

/// Whether `name` must be double-quoted to read back as the same identifier
/// (DuckDB's `KeywordHelper::RequiresQuotes` with its defaults: upper-case letters are
/// allowed, keywords of any category are quoted).
pub fn requires_quotes(name: &str) -> bool {
    let plain = name
        .bytes()
        .enumerate()
        .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()));
    !plain || is_keyword(name)
}

/// `text` wrapped in `quote`, doubling embedded quotes (DuckDB's `WriteQuotedAndEscaped`).
pub fn write_quoted(text: &str, quote: char) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        if c == quote {
            out.push(quote);
        }
        out.push(c);
    }
    out.push(quote);
    out
}

/// An identifier rendered for SQL text, quoted only when needed (DuckDB's `SQLIdentifier`).
pub fn sql_identifier(name: &str) -> Cow<'_, str> {
    if requires_quotes(name) {
        Cow::Owned(write_quoted(name, '"'))
    } else {
        Cow::Borrowed(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_sorted_and_quoted() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(sql_identifier("a_1"), "a_1");
        assert_eq!(sql_identifier("select"), "\"select\"");
        assert_eq!(sql_identifier("My Field"), "\"My Field\"");
        assert_eq!(sql_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(sql_identifier("1a"), "\"1a\"");
        assert_eq!(sql_identifier("key_A"), "key_A");
        assert_eq!(sql_identifier("Select"), "\"Select\"");
        assert_eq!(write_quoted("it's", '\''), "'it''s'");
    }
}
