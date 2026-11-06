//! Raw dictionary keys survive Parquet VARIANT metadata and value conversion.
use rustdb_common::ByteString;
use rustdb_parquet::variant::{VariantMetadata, VariantValue, decode, write::metadata_blob};
use rustdb_types::Value;
#[test]
fn raw_variant_dictionary_keys_round_trip() {
    for byte in 0..=255u8 {
        let key = ByteString::from(vec![b'k', byte, b'x']);
        let encoded = metadata_blob(std::slice::from_ref(&key)).unwrap();
        let metadata = VariantMetadata::new(&encoded).unwrap();
        assert_eq!(metadata.strings, vec![key.clone()]);
        // One OBJECT child with field_id 0, offsets 0..1, primitive true.
        let value = decode(&metadata, &[2, 1, 0, 0, 1, 4], 0).unwrap();
        let VariantValue::Object(fields) = &value else {
            panic!("not an object");
        };
        assert_eq!(
            fields.get(&key),
            Some(&VariantValue::Primitive(Value::boolean(true)))
        );
        let native = rustdb_types::variant_value::to_variant_data(&[value]).unwrap();
        assert_eq!(native[0].as_ref().unwrap().keys, vec![key]);
    }
}
