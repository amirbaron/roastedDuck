//! Native C++ bound-reference equality includes the logical return type.
use rustdb_planner::Expression;
use rustdb_types::LogicalType as L;

#[test]
fn bound_reference_logical_types_match_frozen_cpp() {
    for line in include_str!("data/expression_type_equality_1.5.6.tsv").lines() {
        let (name, expected) = line.split_once('\t').unwrap();
        let (left, right, index) = match name {
            "same_bigint" => (L::BIGINT, L::BIGINT, 0),
            "bigint_integer" => (L::BIGINT, L::INTEGER, 0),
            "date_integer" => (L::DATE, L::INTEGER, 0),
            "decimal_width" => (L::decimal(12, 2), L::decimal(18, 2), 0),
            "varchar_blob" => (L::VARCHAR, L::BLOB, 0),
            "boolean_bigint" => (L::BOOLEAN, L::BIGINT, 0),
            "different_index" => (L::BIGINT, L::BIGINT, 1),
            "timestamp_timezone" => (L::TIMESTAMP, L::TIMESTAMP_TZ, 0),
            _ => panic!("unknown frozen equality fixture"),
        };
        let left = Expression::reference("left", left, 0);
        let right = Expression::reference("right", right, index);
        assert_eq!(left.equals(&right), expected == "1", "{name}");
    }
}
