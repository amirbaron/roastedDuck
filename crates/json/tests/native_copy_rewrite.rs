//! JSON COPY carries the native projection tree and leaves JSON validation to binding.
use roastedduck_ast::ParsedExpressionKind;
use roastedduck_common::{ByteString, CaseInsensitiveMap, Result};
use roastedduck_function::copy::CopyPlanInput;
use roastedduck_types::{LogicalType, Value};
#[test]
fn copy_projection_retains_raw_alias_and_date_format() -> Result<()> {
    let functions = roastedduck_json::json_copy_functions();
    let plan = functions[0].plan.as_ref().unwrap();
    let names = [ByteString::from(b"caf\xe9"), ByteString::from("日付")];
    let types = [LogicalType::INTEGER, LogicalType::DATE];
    let mut options = CaseInsensitiveMap::default();
    options.insert("dateformat", vec![Value::varchar("%Y'%m")]);
    let rewrite = plan(&CopyPlanInput {
        names: &names,
        types: &types,
        options: &options,
    })?;
    assert_eq!(rewrite.format, "csv");
    let ParsedExpressionKind::Function(json) = &rewrite.select_list[0].kind else {
        panic!("not to_json");
    };
    assert_eq!(json.function_name, "to_json");
    let ParsedExpressionKind::Function(pack) = &json.children[0].kind else {
        panic!("not struct_pack");
    };
    assert_eq!(pack.children[0].alias, names[0]);
    assert_eq!(pack.children[1].alias, names[1]);
    let ParsedExpressionKind::Function(date) = &pack.children[1].kind else {
        panic!("not strftime");
    };
    assert_eq!(date.function_name, "strftime");
    let ParsedExpressionKind::Constant(format) = &date.children[1].kind else {
        panic!("not format constant");
    };
    assert_eq!(format.value.as_str(), Some("%Y'%m"));
    Ok(())
}
