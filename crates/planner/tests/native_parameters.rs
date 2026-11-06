//! Frozen C++ native parameter identities and actual shared bind data.
#[path = "../examples/support/native_parameters.rs"]
mod codec;
#[test]
fn native_parameter_wire_and_shared_data_match_cpp() {
    for line in include_str!("data/native_parameters_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}

#[test]
fn binding_replacement_keeps_raw_parameter_text() {
    use rustdb_ast::{ParsedExpression, ParsedExpressionKind};
    use rustdb_planner::{expression_binder::set_bound, prepared::BoundParameterMap};
    for name in [b"\x80".as_slice(), b"caf\xe9", b"a\xffb", "雪".as_bytes()] {
        let mut parameters = BoundParameterMap::default();
        let bound = parameters.bind_parameter_expression(name, name);
        let expected = bound.to_bytes();
        let mut parsed = ParsedExpression::column_ref(["placeholder"]);
        parsed.alias = "alias".into();
        set_bound(&mut parsed, bound);
        assert_eq!(parsed.alias, "alias");
        let ParsedExpressionKind::Bound(bound) = parsed.kind else {
            panic!("not bound")
        };
        assert_eq!(bound.text, expected);
    }
}
