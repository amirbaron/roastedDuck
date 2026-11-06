//! Behaviour of the hand-written serialization code that the DuckDB fixtures do not reach:
//! version-dependent layouts and legacy forms.

use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::json::{JsonDeserializer, JsonOptions, JsonSerializer, write_json};
use rustdb_serde::{DeserializeObject, SerializationCompatibility, SerializationOptions, SerializeObject};
use rustdb_types::{Value, expression_ids};

use crate::{
    AggregateHandling, CommonTableExpressionMap, ExpressionClass, ExpressionType, MacroFunction,
    MacroFunctionKind, ParsedExpression, ParsedExpressionKind, QueryNode, QueryNodeKind, ScalarMacroFunction,
    SelectNode, SetOperationNode, SetOperationType, TableRef, TableRefKind,
};

fn options(compatibility: SerializationCompatibility) -> SerializationOptions {
    SerializationOptions {
        compatibility,
        ..SerializationOptions::default()
    }
}

fn json<T: SerializeObject>(value: &T, compatibility: SerializationCompatibility) -> String {
    let tree = JsonSerializer::serialize(value, JsonOptions::default(), options(compatibility)).unwrap();
    write_json(&tree, false)
}

fn from_json<T: DeserializeObject>(text: &str) -> T {
    let tree = rustdb_serde::json::parse_json(text).unwrap();
    JsonDeserializer::deserialize(&tree).unwrap()
}

fn select_constant(value: i32) -> QueryNode {
    QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list: vec![ParsedExpression::constant(Value::integer(value))],
            from_table: Box::new(TableRef {
                external_dependency: crate::ExternalDependency::default(),
                alias: rustdb_common::ByteString::new(),
                sample: None,
                query_location: None,
                kind: TableRefKind::Empty,
            }),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        })),
    }
}

#[test]
fn external_scan_ownership_survives_copies_without_changing_serialization() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Owner(Arc<AtomicUsize>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(Owner(Arc::clone(&drops)));
    let mut table = TableRef {
        alias: "external".into(),
        sample: None,
        query_location: None,
        external_dependency: crate::ExternalDependency::default(),
        kind: TableRefKind::Empty,
    };
    let plain_json = json(&table, SerializationCompatibility::latest());
    table.external_dependency = crate::ExternalDependency::new(owner);
    let copied = table.clone();
    assert!(!copied.external_dependency.is_empty());
    assert_eq!(json(&copied, SerializationCompatibility::latest()), plain_json);
    let decoded: TableRef = from_json(&plain_json);
    assert!(decoded.external_dependency.is_empty());
    assert_eq!(decoded, copied);
    drop(table);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(copied);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn retaining_external_scans_preserves_unbound_expressions_and_sampling() {
    let mut original = crate::SelectStatement {
        node: Box::new(select_constant(42)),
        named_param_map: rustdb_common::CaseInsensitiveMap::default(),
    };
    let sample = Some(Box::new(crate::SampleOptions {
        sample_size: Value::integer(3),
        is_percentage: false,
        method: crate::SampleMethod::ReservoirSample,
        seed: Some(1),
        repeatable: true,
    }));
    let QueryNodeKind::Select(node) = &mut original.node.kind else {
        panic!("Expected SELECT");
    };
    node.from_table.kind = TableRefKind::BaseTable(Box::new(crate::BaseTableRef {
        table_name: "external".into(),
        schema_name: rustdb_common::ByteString::default(),
        catalog_name: rustdb_common::ByteString::default(),
        column_name_alias: Vec::new(),
        at_clause: None,
    }));
    node.from_table.query_location = Some(9);
    node.from_table.sample.clone_from(&sample);
    let expressions = node.select_list.clone();
    let mut bound = original.clone();
    let QueryNodeKind::Select(node) = &mut bound.node.kind else {
        panic!("Expected SELECT");
    };
    node.select_list = vec![ParsedExpression::constant(Value::integer(999))];
    node.from_table.sample = None;
    node.from_table.external_dependency = crate::ExternalDependency::new(std::sync::Arc::new(()));
    node.from_table.kind = TableRefKind::Subquery(crate::SubqueryRef {
        subquery: Box::new(crate::SelectStatement {
            node: Box::new(select_constant(7)),
            named_param_map: rustdb_common::CaseInsensitiveMap::default(),
        }),
        column_name_alias: Vec::new(),
    });
    crate::retain_external_replacements(&mut original, &mut bound);
    let QueryNodeKind::Select(node) = &original.node.kind else {
        panic!("Expected SELECT");
    };
    assert_eq!(node.select_list, expressions);
    assert_eq!(node.from_table.sample, sample);
    assert!(matches!(node.from_table.kind, TableRefKind::Subquery(_)));
    assert!(!node.from_table.external_dependency.is_empty());
}

#[test]
fn unbound_type_expression_ids_match_the_generated_enums() {
    let class = |id: (&str, u8)| (ExpressionClass::from_name(id.0), ExpressionClass::from_repr(id.1));
    let ty = |id: (&str, u8)| (ExpressionType::from_name(id.0), ExpressionType::from_repr(id.1));
    let c = class(expression_ids::CLASS_CONSTANT);
    assert_eq!(
        c,
        (Some(ExpressionClass::Constant), Some(ExpressionClass::Constant))
    );
    let c = class(expression_ids::CLASS_TYPE);
    assert_eq!(c, (Some(ExpressionClass::Type), Some(ExpressionClass::Type)));
    let t = ty(expression_ids::TYPE_VALUE_CONSTANT);
    assert_eq!(
        t,
        (
            Some(ExpressionType::ValueConstant),
            Some(ExpressionType::ValueConstant)
        )
    );
    let t = ty(expression_ids::TYPE_TYPE);
    assert_eq!(t, (Some(ExpressionType::Type), Some(ExpressionType::Type)));
}

#[test]
fn set_operations_nest_for_old_versions_and_list_children_for_new_ones() {
    let union = QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::SetOperation(SetOperationNode {
            setop_type: SetOperationType::Union,
            setop_all: true,
            children: vec![select_constant(1), select_constant(2), select_constant(3)],
        }),
    };
    let old = json(&union, SerializationCompatibility::default());
    assert!(old.contains(r#""right":{"type":"SET_OPERATION_NODE""#), "{old}");
    assert!(!old.contains(r#""children""#), "{old}");
    let new = json(&union, SerializationCompatibility::latest());
    assert!(new.contains(r#""left":null,"right":null"#), "{new}");
    assert!(new.contains(r#""children":[{"type":"SELECT_NODE""#), "{new}");

    // The latest form round-trips; the old form reads back as nested binary operations.
    assert_eq!(from_json::<QueryNode>(&new), union);
    let nested: QueryNode = from_json(&old);
    let QueryNodeKind::SetOperation(outer) = &nested.kind else {
        panic!("not a set operation")
    };
    assert_eq!(outer.children.len(), 2);
    assert_eq!(json(&nested, SerializationCompatibility::default()), old);

    let bytes = BinarySerializer::serialize(&union, options(SerializationCompatibility::latest())).unwrap();
    assert_eq!(
        BinaryDeserializer::deserialize::<QueryNode>(&bytes).unwrap(),
        union
    );
}

#[test]
fn legacy_unpacked_stars_become_unpack_operators() {
    let legacy = r#"{"class":"STAR","type":"STAR","alias":"x","relation_name":"","exclude_list":[],"columns":true,"unpacked":true}"#;
    let expression: ParsedExpression = from_json(legacy);
    assert_eq!(expression.expression_type, ExpressionType::OperatorUnpack);
    assert_eq!(expression.alias, "x");
    let ParsedExpressionKind::Operator(operator) = &expression.kind else {
        panic!("not an operator: {expression:?}")
    };
    assert!(matches!(&operator.children[0].kind, ParsedExpressionKind::Star(star) if star.columns));
}

#[test]
fn macros_from_old_versions_list_default_parameters() {
    // Before serialization version 6, parameters with defaults were only in the defaults map.
    let legacy = r#"{"type":"SCALAR_MACRO","parameters":[{"class":"COLUMN_REF","type":"COLUMN_REF","column_names":["a"]}],"default_parameters":[{"key":"b","value":{"class":"CONSTANT","type":"VALUE_CONSTANT","value":{"type":{"id":"INTEGER"},"is_null":false,"value":5}}}],"expression":{"class":"COLUMN_REF","type":"COLUMN_REF","column_names":["a"]}}"#;
    let function: MacroFunction = from_json(legacy);
    assert_eq!(function.parameters.len(), 2);
    assert_eq!(function.parameters[1], ParsedExpression::column_ref(["b"]));
    assert_eq!(function.types.len(), 2);
    assert!(matches!(
        function.kind,
        MacroFunctionKind::Scalar(ScalarMacroFunction { .. })
    ));

    let old = json(&function, SerializationCompatibility::default());
    assert!(old.contains(r#""parameters":[{"class":"COLUMN_REF","type":"COLUMN_REF","alias":"","query_location":18446744073709551615,"column_names":["a"]}]"#), "{old}");
    assert!(!old.contains(r#""types""#), "{old}");
    let new = json(&function, SerializationCompatibility::latest());
    assert!(new.contains(r#""column_names":["b"]"#), "{new}");
    assert!(
        new.contains(r#""types":[{"id":"UNKNOWN","type_info":null},{"id":"UNKNOWN","type_info":null}]"#),
        "{new}"
    );
}
