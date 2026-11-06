//! The TPC-DS queries and their answers (DuckDB's `dsdgen/include/tpcds_constants.hpp`,
//! generated from `dsdgen/queries` and `dsdgen/answers`, which are embedded here as
//! DuckDB embeds them).

/// `TPCDS_QUERIES_COUNT`.
pub const TPCDS_QUERIES_COUNT: usize = 99;

/// The text of the files `NN.<extension>` of `dir` for the given query numbers.
macro_rules! per_query {
    ($dir:literal, $extension:literal, [$($query:literal),*]) => {
        [$(include_str!(concat!($dir, "/", $query, ".", $extension))),*]
    };
}

/// The text of a file of `dir` per query, `01` ... `99`.
macro_rules! all_queries {
    ($dir:literal, $extension:literal) => {
        per_query!(
            $dir,
            $extension,
            [
                "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15",
                "16", "17", "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30",
                "31", "32", "33", "34", "35", "36", "37", "38", "39", "40", "41", "42", "43", "44", "45",
                "46", "47", "48", "49", "50", "51", "52", "53", "54", "55", "56", "57", "58", "59", "60",
                "61", "62", "63", "64", "65", "66", "67", "68", "69", "70", "71", "72", "73", "74", "75",
                "76", "77", "78", "79", "80", "81", "82", "83", "84", "85", "86", "87", "88", "89", "90",
                "91", "92", "93", "94", "95", "96", "97", "98", "99"
            ]
        )
    };
}

/// `TPCDS_QUERIES`.
pub const TPCDS_QUERIES: [&str; TPCDS_QUERIES_COUNT] = all_queries!("../queries", "sql");
/// `TPCDS_ANSWERS_SF0_01`: the answers as CSV with `|` separators.
pub const TPCDS_ANSWERS_SF0_01: [&str; TPCDS_QUERIES_COUNT] = all_queries!("../answers/sf0.01", "csv");
/// `TPCDS_ANSWERS_SF1`.
pub const TPCDS_ANSWERS_SF1: [&str; TPCDS_QUERIES_COUNT] = all_queries!("../answers/sf1", "csv");
