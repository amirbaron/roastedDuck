//! Valid JSON escaped keys retain native bytes when cast into VARIANT dictionaries.
use rustdb_common::{ByteString, Result};
use rustdb_function::cast::{CastFunctionSet, CastParameters};
use rustdb_types::{LogicalType, Value, Vector, variant::VariantData};
#[test]
fn escaped_control_and_unicode_keys_reach_variant_dictionary() -> Result<()> {
    let mut casts = CastFunctionSet::new();
    rustdb_json::register_json_casts(&mut casts);
    let cast = casts.get_cast_function(&rustdb_json::json_type(), &LogicalType::variant())?;
    let mut keys: Vec<ByteString> = (0..=127u8).map(|byte| vec![b'k', byte, b'x'].into()).collect();
    keys.extend([ByteString::from("café"), ByteString::from("鍵")]);
    for key in keys {
        let mut text = vec![b'{'];
        rustdb_json::yyjson::write_string(&mut text, &key);
        text.extend_from_slice(b":1}");
        let mut source = Vector::flat(rustdb_json::json_type(), 1);
        source.set_value(0, &Value::bytes(rustdb_json::json_type(), &text))?;
        let mut result = Vector::flat(LogicalType::variant(), 1);
        assert!(cast.execute(&source, &mut result, 1, &mut CastParameters::new())?);
        let data = VariantData::from_vector(&result, 0)?.unwrap();
        assert_eq!(data.keys, vec![key]);
    }
    Ok(())
}
