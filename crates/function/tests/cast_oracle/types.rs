//! The types of the cast grid, keyed by the SQL spelling used in oracle scripts and data
//! files.

use std::sync::OnceLock;

use rustdb_types::LogicalType;

/// Broad kind of a grid type; selects which VARCHAR inputs are interesting for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// The type of the `NULL` literal (source only).
    Null,
    Boolean,
    Integer,
    Float,
    Decimal,
    Varchar,
    Blob,
    Bit,
    Uuid,
    Temporal,
    Enum,
    Nested,
}

/// A type of the grid.
#[derive(Debug)]
pub struct GridType {
    /// SQL spelling, as written in oracle scripts and data files.
    pub sql: &'static str,
    pub ty: LogicalType,
    pub family: Family,
}

/// Grid types are identified by their SQL spelling.
impl PartialEq for GridType {
    fn eq(&self, other: &Self) -> bool {
        self.sql == other.sql
    }
}

impl Eq for GridType {}

impl std::hash::Hash for GridType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.sql.hash(state);
    }
}

/// The decimal types of the grid: widths 4/9/18/38 with several scales.
pub const DECIMALS: [(u8, u8); 11] = [
    (4, 0),
    (4, 1),
    (4, 4),
    (9, 0),
    (9, 3),
    (18, 0),
    (18, 6),
    (18, 18),
    (38, 0),
    (38, 10),
    (38, 38),
];

fn build() -> Vec<GridType> {
    use Family as F;
    let simple = |sql: &'static str, ty: LogicalType, family: Family| GridType { sql, ty, family };
    let mut types = vec![
        simple("NULL", LogicalType::SQLNULL, F::Null),
        simple("BOOLEAN", LogicalType::BOOLEAN, F::Boolean),
        simple("TINYINT", LogicalType::TINYINT, F::Integer),
        simple("SMALLINT", LogicalType::SMALLINT, F::Integer),
        simple("INTEGER", LogicalType::INTEGER, F::Integer),
        simple("BIGINT", LogicalType::BIGINT, F::Integer),
        simple("HUGEINT", LogicalType::HUGEINT, F::Integer),
        simple("UTINYINT", LogicalType::UTINYINT, F::Integer),
        simple("USMALLINT", LogicalType::USMALLINT, F::Integer),
        simple("UINTEGER", LogicalType::UINTEGER, F::Integer),
        simple("UBIGINT", LogicalType::UBIGINT, F::Integer),
        simple("UHUGEINT", LogicalType::UHUGEINT, F::Integer),
        simple("FLOAT", LogicalType::FLOAT, F::Float),
        simple("DOUBLE", LogicalType::DOUBLE, F::Float),
    ];
    for (width, scale) in DECIMALS {
        let sql: &'static str = format!("DECIMAL({width},{scale})").leak();
        types.push(simple(sql, LogicalType::decimal(width, scale), F::Decimal));
    }
    let child = |name: &str, ty: LogicalType| (name.to_owned(), ty);
    types.extend([
        simple("VARCHAR", LogicalType::VARCHAR, F::Varchar),
        simple("BLOB", LogicalType::BLOB, F::Blob),
        simple("BIT", LogicalType::BIT, F::Bit),
        simple("UUID", LogicalType::UUID, F::Uuid),
        simple("DATE", LogicalType::DATE, F::Temporal),
        simple("TIME", LogicalType::TIME, F::Temporal),
        simple("TIME_NS", LogicalType::TIME_NS, F::Temporal),
        simple("TIMETZ", LogicalType::TIME_TZ, F::Temporal),
        simple("TIMESTAMP", LogicalType::TIMESTAMP, F::Temporal),
        simple("TIMESTAMP_S", LogicalType::TIMESTAMP_S, F::Temporal),
        simple("TIMESTAMP_MS", LogicalType::TIMESTAMP_MS, F::Temporal),
        simple("TIMESTAMP_NS", LogicalType::TIMESTAMP_NS, F::Temporal),
        simple("TIMESTAMPTZ", LogicalType::TIMESTAMP_TZ, F::Temporal),
        simple("INTERVAL", LogicalType::INTERVAL, F::Temporal),
        simple(
            "ENUM('a', 'b', 'c')",
            LogicalType::enum_type(vec!["a".into(), "b".into(), "c".into()]),
            F::Enum,
        ),
        simple("INTEGER[]", LogicalType::list(LogicalType::INTEGER), F::Nested),
        simple("VARCHAR[]", LogicalType::list(LogicalType::VARCHAR), F::Nested),
        simple(
            "INTEGER[2]",
            LogicalType::array(LogicalType::INTEGER, 2),
            F::Nested,
        ),
        simple(
            "STRUCT(a INTEGER, b VARCHAR)",
            LogicalType::struct_type(vec![
                child("a", LogicalType::INTEGER),
                child("b", LogicalType::VARCHAR),
            ]),
            F::Nested,
        ),
        simple(
            "MAP(VARCHAR, INTEGER)",
            LogicalType::map(LogicalType::VARCHAR, LogicalType::INTEGER),
            F::Nested,
        ),
        simple(
            "UNION(i INTEGER, s VARCHAR)",
            LogicalType::union_type(vec![
                child("i", LogicalType::INTEGER),
                child("s", LogicalType::VARCHAR),
            ]),
            F::Nested,
        ),
    ]);
    types
}

/// Every grid type, in grid order (`NULL` first).
pub fn all() -> &'static [GridType] {
    static TYPES: OnceLock<Vec<GridType>> = OnceLock::new();
    TYPES.get_or_init(build)
}

/// The cast targets: every grid type except the `NULL` literal type.
pub fn targets() -> impl Iterator<Item = &'static GridType> {
    all().iter().filter(|t| t.family != Family::Null)
}

/// Looks a grid type up by its SQL spelling.
pub fn lookup(sql: &str) -> Option<&'static GridType> {
    all().iter().find(|t| t.sql == sql)
}

/// Looks a grid type up by its SQL spelling, panicking on unknown names.
#[expect(clippy::panic, reason = "test helper: an unknown name is a bug in the grid")]
pub fn get(sql: &str) -> &'static GridType {
    lookup(sql).unwrap_or_else(|| panic!("unknown grid type {sql}"))
}
