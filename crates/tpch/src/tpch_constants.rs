//! The TPC-H queries and their answers (DuckDB's `dbgen/include/tpch_constants.hpp`,
//! generated from `dbgen/queries` and `dbgen/answers`, which are embedded here as is).

/// `TPCH_QUERIES_COUNT`.
pub const TPCH_QUERIES_COUNT: usize = 22;

/// The text of a file of `dir` per query, `q01` ... `q22`.
macro_rules! per_query {
    ($dir:literal, $extension:literal) => {
        [
            include_str!(concat!($dir, "/q01.", $extension)),
            include_str!(concat!($dir, "/q02.", $extension)),
            include_str!(concat!($dir, "/q03.", $extension)),
            include_str!(concat!($dir, "/q04.", $extension)),
            include_str!(concat!($dir, "/q05.", $extension)),
            include_str!(concat!($dir, "/q06.", $extension)),
            include_str!(concat!($dir, "/q07.", $extension)),
            include_str!(concat!($dir, "/q08.", $extension)),
            include_str!(concat!($dir, "/q09.", $extension)),
            include_str!(concat!($dir, "/q10.", $extension)),
            include_str!(concat!($dir, "/q11.", $extension)),
            include_str!(concat!($dir, "/q12.", $extension)),
            include_str!(concat!($dir, "/q13.", $extension)),
            include_str!(concat!($dir, "/q14.", $extension)),
            include_str!(concat!($dir, "/q15.", $extension)),
            include_str!(concat!($dir, "/q16.", $extension)),
            include_str!(concat!($dir, "/q17.", $extension)),
            include_str!(concat!($dir, "/q18.", $extension)),
            include_str!(concat!($dir, "/q19.", $extension)),
            include_str!(concat!($dir, "/q20.", $extension)),
            include_str!(concat!($dir, "/q21.", $extension)),
            include_str!(concat!($dir, "/q22.", $extension)),
        ]
    };
}

/// `TPCH_QUERIES`.
pub const TPCH_QUERIES: [&str; TPCH_QUERIES_COUNT] = per_query!("../queries", "sql");
/// `TPCH_ANSWERS_SF0_01`: the answers as CSV with `|` separators.
pub const TPCH_ANSWERS_SF0_01: [&str; TPCH_QUERIES_COUNT] = per_query!("../answers/sf0.01", "csv");
/// `TPCH_ANSWERS_SF0_1`.
pub const TPCH_ANSWERS_SF0_1: [&str; TPCH_QUERIES_COUNT] = per_query!("../answers/sf0.1", "csv");
/// `TPCH_ANSWERS_SF1`.
pub const TPCH_ANSWERS_SF1: [&str; TPCH_QUERIES_COUNT] = per_query!("../answers/sf1", "csv");
