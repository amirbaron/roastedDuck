use roastedduck_postgres::codec::{Codec, Options, encode_parameter};
use roastedduck_types::{LogicalType, Value};
use tokio_postgres::types::Type;

#[test]
fn fixed_width_fields_reject_truncation_and_trailing_bytes() {
    for (ty, size) in [
        (Type::BOOL, 1),
        (Type::INT2, 2),
        (Type::INT4, 4),
        (Type::INT8, 8),
        (Type::FLOAT4, 4),
        (Type::FLOAT8, 8),
        (Type::DATE, 4),
        (Type::TIMESTAMP, 8),
        (Type::TIMESTAMPTZ, 8),
        (Type::TIMETZ, 12),
        (Type::INTERVAL, 16),
        (Type::UUID, 16),
    ] {
        let codec = Codec::new(&ty, -1, 0, Options::default()).unwrap();
        for length in 0..=size + 1 {
            let result = codec.decode(Some(&vec![0; length]));
            assert_eq!(result.is_ok(), length == size, "{ty}, {length}");
        }
    }
}

#[test]
fn malformed_array_shape_cannot_trigger_unbounded_allocation() {
    let codec = Codec::new(&Type::INT4_ARRAY, -1, 2, Options::default()).unwrap();
    // Huge outer dimension followed by an empty inner dimension used to pass a
    // product-only bounds check (product zero) before reserving the outer list.
    let mut bytes = Vec::new();
    for n in [2u32, 0, 23, u32::MAX, 1, 0, 1] {
        bytes.extend_from_slice(&n.to_be_bytes());
    }
    assert!(codec.decode(Some(&bytes)).is_err());
    let valid = Value::list(
        LogicalType::list(LogicalType::INTEGER),
        vec![Value::list(
            LogicalType::INTEGER,
            vec![Value::integer(1), Value::integer(2)],
        )],
    );
    let bytes = encode_parameter(&valid).unwrap().0.unwrap();
    for length in 0..bytes.len() {
        assert!(codec.decode(Some(&bytes[..length])).is_err(), "{length}");
    }
    assert_eq!(codec.decode(Some(&bytes)).unwrap(), valid);
}

#[test]
fn ragged_and_null_subarrays_rejected_before_sending() {
    let row = Value::list(LogicalType::INTEGER, vec![Value::integer(1)]);
    for second in [
        Value::list(LogicalType::INTEGER, vec![]),
        Value::null(LogicalType::list(LogicalType::INTEGER)),
    ] {
        let value = Value::list(LogicalType::list(LogicalType::INTEGER), vec![row.clone(), second]);
        assert!(encode_parameter(&value).is_err());
    }
}

#[test]
fn query_oid_mapping_is_narrower_than_catalog_mapping() {
    for ty in [Type::OID, Type::POINT, Type::POLYGON, Type::BYTEA_ARRAY] {
        assert!(
            !Codec::new(&ty, -1, 0, Options::default())
                .unwrap()
                .needs_text_cast()
        );
        assert!(
            Codec::for_query(&ty, -1, Options::default())
                .unwrap()
                .needs_text_cast()
        );
    }
}
