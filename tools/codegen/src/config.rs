//! What to generate where, and the Rust-specific overrides on top of DuckDB's specs.
//!
//! DuckDB's specs describe the serialized form; they don't say which pointers may be
//! NULL, how C++ member names map to idiomatic Rust, or which classes have hand-written
//! (`custom_implementation`-style) code. Those decisions live here.
//!
//! Most classes become Rust types of the same shape. The logical plan's operators and
//! expressions do not: rustdb's are shaped for planning, so their specs become property
//! structs ([`Content::Properties`]) that hand-written `Serialize` impls fill in.

/// A crate receiving generated code under `src/generated/`.
#[derive(Debug)]
pub struct Target {
    /// Crate directory, relative to the workspace root.
    pub crate_dir: &'static str,
    /// The crate's name in Rust paths (`rustdb_types`).
    pub crate_name: &'static str,
    /// Derives on every generated struct and subclass enum (besides `Clone, Debug`).
    pub derives: &'static [&'static str],
    pub modules: &'static [Module],
}

/// One generated file (`src/generated/<name>.rs`).
#[derive(Debug)]
pub struct Module {
    pub name: &'static str,
    /// What the module contains.
    pub content: Content,
}

#[derive(Debug)]
pub enum Content {
    /// Classes from a spec file; `None` selects all classes of the file.
    Classes {
        spec: &'static str,
        classes: Option<&'static [&'static str]>,
    },
    /// Property structs: for each class of a spec file (`None`: all of them) a struct of
    /// its members as the serializer writes them, borrowed, with its `SerializeObject`
    /// impl. The class's Rust type is hand-written; its `Serialize` impl fills one in.
    /// Write-only, like DuckDB's plans (1.5.6 has no `json_deserialize_plan`).
    Properties {
        spec: &'static str,
        classes: Option<&'static [&'static str]>,
    },
    /// C++ enums with their `EnumUtil` string tables.
    Enums(&'static [&'static str]),
    /// The serialization version table from `version_map.json`.
    VersionMap,
}

/// Overrides for one spec class.
#[derive(Debug)]
pub struct ClassConfig {
    pub class: &'static str,
    /// Rust name, if not derived from the C++ name.
    pub rust_name: Option<&'static str>,
    /// The type and its `SerializeObject`/`DeserializeObject` impls are hand-written.
    pub custom: bool,
    /// Hierarchy roots: the name of the enum of subclasses (default `<Name>Kind`).
    pub kind_name: Option<&'static str>,
    /// Hierarchy roots: suffix removed from `class_type` values to name the variants.
    pub strip_suffix: Option<&'static str>,
    /// Hierarchy roots: explicit variant names, by `class_type` value.
    pub variant_names: &'static [(&'static str, &'static str)],
    /// Hierarchy roots: subclasses (by `class_type` value) whose deserialization yields no
    /// object at all (C++ `custom_switch_code` returning `nullptr`); the root's
    /// deserializer then returns `Option<Self>`.
    pub null_variants: &'static [&'static str],
    /// Subclasses: Rust expression replacing the base's deserialization switch arm
    /// (C++ `custom_switch_code`). It may use `deserializer` and the base members'
    /// local variables, and must evaluate to the kind enum (or `return` early).
    pub deserialize_arm: Option<&'static str>,
    /// Subclasses: no variant in the kind enum (classes that only exist to read old data).
    pub no_variant: bool,
    /// Where the spec's C++ `finalize_deserialization` code is ported, if not in a custom
    /// class (the generator refuses unported custom C++ code).
    pub finalize_ported_in: Option<&'static str>,
    pub fields: &'static [FieldConfig],
    /// Additional derives.
    pub derives: &'static [&'static str],
    /// Hierarchy roots: variants for C++ subclasses that have no serialization spec
    /// because they never reach a serializer.
    pub extra_variants: &'static [ExtraVariant],
    /// In-memory members that are not serialized (such as the parsed options of a COPY,
    /// which the binder turns into the serialized ones).
    pub extra_fields: &'static [ExtraField],
    /// Lay the struct out like the C++ one (`#[repr(C)]`), for types stored in vectors
    /// whose bytes are read field by field (`interval_t`).
    pub repr_c: bool,
    /// Property structs: the hand-written type holding the class's data (a path from its
    /// crate's root), as members of other classes refer to it.
    pub data_type: Option<&'static str>,
}

impl ClassConfig {
    pub const DEFAULT: Self = Self {
        class: "",
        rust_name: None,
        custom: false,
        kind_name: None,
        strip_suffix: None,
        variant_names: &[],
        null_variants: &[],
        deserialize_arm: None,
        no_variant: false,
        finalize_ported_in: None,
        fields: &[],
        derives: &[],
        extra_variants: &[],
        extra_fields: &[],
        repr_c: false,
        data_type: None,
    };
}

/// A member of the C++ class that its serialization spec leaves out; deserialization
/// initializes it with `default`.
#[derive(Debug)]
pub struct ExtraField {
    pub name: &'static str,
    /// The field type, as a Rust path.
    pub ty: &'static str,
    pub doc: &'static str,
    /// Rust expression of the value after deserialization.
    pub default: &'static str,
}

/// A hand-written variant of a kind enum, for a C++ subclass without a serialization spec
/// (such as the binder's in-place `BoundExpression`). Deserializing its discriminant keeps
/// failing like any other unsupported value.
#[derive(Debug)]
pub struct ExtraVariant {
    pub variant: &'static str,
    /// The payload type, as a Rust path.
    pub ty: &'static str,
    /// The discriminant value (C++ enum member) of the subclass.
    pub discriminant: &'static str,
    pub doc: &'static str,
    /// Rust expression returned by `serialize_fields` for this variant.
    pub serialize: &'static str,
}

/// Overrides for one member.
#[derive(Debug)]
pub struct FieldConfig {
    /// The spec member name.
    pub member: &'static str,
    /// Rust field name, if not the spec member name.
    pub rename: Option<&'static str>,
    /// A pointer that is never NULL in a well-formed tree: `Box<T>` instead of `Option<Box<T>>`.
    pub required: bool,
    /// Rust expression computing the value to write (C++ `serialize_property` methods);
    /// may use `self` and `serializer`.
    pub serialize_with: Option<&'static str>,
    /// Property structs: the field's Rust type, where the hand-written type holds the
    /// member as another type that writes it the same way (a `usize` index, or a type
    /// of a crate the class's crate does not depend on).
    pub property_type: Option<&'static str>,
}

impl FieldConfig {
    pub const DEFAULT: Self = Self {
        member: "",
        rename: None,
        required: false,
        serialize_with: None,
        property_type: None,
    };
}

const fn required(member: &'static str) -> FieldConfig {
    FieldConfig {
        member,
        required: true,
        ..FieldConfig::DEFAULT
    }
}

const fn rename(member: &'static str, to: &'static str) -> FieldConfig {
    FieldConfig {
        member,
        rename: Some(to),
        ..FieldConfig::DEFAULT
    }
}

const fn property_type(member: &'static str, ty: &'static str) -> FieldConfig {
    FieldConfig {
        member,
        property_type: Some(ty),
        ..FieldConfig::DEFAULT
    }
}

/// Types that are not generated, with the crate that defines them (re-exported at its
/// root) and how they are written.
pub const EXTERNAL_TYPES: &[(&str, &str, &str)] = &[
    ("LogicalType", "rustdb_types", "LogicalType"),
    ("Value", "rustdb_types", "Value"),
    ("ColumnDataCollection", "rustdb_types", "ColumnDataCollection"),
    (
        "CoordinateReferenceSystem",
        "rustdb_types",
        "CoordinateReferenceSystem",
    ),
    ("LogicalIndex", "rustdb_common", "LogicalIndex"),
    ("PhysicalIndex", "rustdb_common", "PhysicalIndex"),
];

/// Every generated file.
pub const TARGETS: &[Target] = &[
    Target {
        crate_dir: "crates/serde",
        crate_name: "rustdb_serde",
        derives: &[],
        modules: &[Module {
            name: "version_map",
            content: Content::VersionMap,
        }],
    },
    Target {
        crate_dir: "crates/types",
        crate_name: "rustdb_types",
        // Types are compared and hashed (type maps, catalog lookups).
        derives: &["PartialEq", "Eq", "Hash"],
        modules: &[
            Module {
                name: "enums",
                content: Content::Enums(&[
                    "LogicalTypeId",
                    "ExtraTypeInfoType",
                    "GeometryStorageType",
                    "ColumnIndexType",
                ]),
            },
            Module {
                name: "type_info",
                content: Content::Classes {
                    spec: "types.json",
                    classes: None,
                },
            },
            Module {
                name: "interval",
                content: Content::Classes {
                    spec: "nodes.json",
                    classes: Some(&["interval_t"]),
                },
            },
            Module {
                name: "column_index",
                content: Content::Properties {
                    spec: "nodes.json",
                    classes: Some(&["ColumnIndex"]),
                },
            },
        ],
    },
    Target {
        crate_dir: "crates/ast",
        crate_name: "rustdb_ast",
        derives: &["PartialEq"],
        modules: &[
            Module {
                name: "enums",
                content: Content::Enums(&[
                    "AggregateHandling",
                    "AlterDatabaseType",
                    "AlterForeignKeyType",
                    "AlterTableType",
                    "AlterType",
                    "AlterViewType",
                    "CTEMaterialize",
                    "CatalogType",
                    "CompressionType",
                    "ConstraintType",
                    "DeprecatedIndexType",
                    "ExplainFormat",
                    "ExplainType",
                    "ExpressionClass",
                    "ExpressionType",
                    "ExtraDropInfoType",
                    "ForeignKeyType",
                    "IndexConstraintType",
                    "InsertColumnOrder",
                    "JoinRefType",
                    "JoinType",
                    "LambdaSyntaxType",
                    "LoadType",
                    "MacroType",
                    "MergeActionCondition",
                    "MergeActionType",
                    "OnConflictAction",
                    "OnCreateConflict",
                    "OnEntryNotFound",
                    "OrderByNullType",
                    "OrderType",
                    "OrdinalityType",
                    "ParseInfoType",
                    "QueryNodeType",
                    "ResultModifierType",
                    "SampleMethod",
                    "SecretPersistType",
                    "SetOperationType",
                    "SetScope",
                    "SetType",
                    "ShowType",
                    "StatementType",
                    "SubqueryType",
                    "TableColumnType",
                    "TableReferenceType",
                    "TransactionInvalidationPolicy",
                    "TransactionModifierType",
                    "TransactionType",
                    "WindowBoundary",
                    "WindowExcludeMode",
                ]),
            },
            Module {
                name: "parsed_expression",
                content: Content::Classes {
                    spec: "parsed_expression.json",
                    classes: None,
                },
            },
            Module {
                name: "query_node",
                content: Content::Classes {
                    spec: "query_node.json",
                    classes: None,
                },
            },
            Module {
                name: "result_modifier",
                content: Content::Classes {
                    spec: "result_modifier.json",
                    classes: Some(&[
                        "ResultModifier",
                        "LimitModifier",
                        "DistinctModifier",
                        "OrderModifier",
                        "LimitPercentModifier",
                    ]),
                },
            },
            Module {
                name: "tableref",
                content: Content::Classes {
                    spec: "tableref.json",
                    classes: Some(&[
                        "TableRef",
                        "BaseTableRef",
                        "JoinRef",
                        "SubqueryRef",
                        "TableFunctionRef",
                        "EmptyTableRef",
                        "ExpressionListRef",
                        "ColumnDataRef",
                        "PivotRef",
                        "ShowRef",
                        "AtClause",
                    ]),
                },
            },
            Module {
                name: "statement",
                content: Content::Classes {
                    spec: "statement.json",
                    classes: None,
                },
            },
            Module {
                name: "nodes",
                content: Content::Classes {
                    spec: "nodes.json",
                    classes: Some(&[
                        "CommonTableExpressionInfo",
                        "CommonTableExpressionMap",
                        "OrderByNode",
                        "CaseCheck",
                        "SampleOptions",
                        "PivotColumn",
                        "PivotColumnEntry",
                        "ColumnDefinition",
                        "ColumnList",
                        "QualifiedColumnName",
                        "VacuumOptions",
                        "ExportedTableInfo",
                        "ExportedTableData",
                    ]),
                },
            },
            Module {
                name: "constraint",
                content: Content::Classes {
                    spec: "constraint.json",
                    classes: None,
                },
            },
            Module {
                name: "create_info",
                content: Content::Classes {
                    spec: "create_info.json",
                    classes: None,
                },
            },
            Module {
                name: "parse_info",
                content: Content::Classes {
                    spec: "parse_info.json",
                    classes: None,
                },
            },
            Module {
                name: "macro_function",
                content: Content::Classes {
                    spec: "macro_function.json",
                    classes: None,
                },
            },
            Module {
                name: "extra_drop_info",
                content: Content::Classes {
                    spec: "extra_drop_info.json",
                    classes: None,
                },
            },
            Module {
                name: "dependency",
                content: Content::Classes {
                    spec: "dependency.json",
                    classes: None,
                },
            },
        ],
    },
    Target {
        crate_dir: "crates/storage",
        crate_name: "rustdb_storage",
        derives: &[],
        modules: &[
            Module {
                name: "enums",
                content: Content::Enums(&[
                    "TableFilterType",
                    "OrderByStatistics",
                    "OrderByColumnType",
                    "StorageIndexType",
                ]),
            },
            Module {
                name: "table_filter",
                content: Content::Properties {
                    spec: "table_filter.json",
                    classes: None,
                },
            },
            Module {
                name: "nodes",
                content: Content::Properties {
                    spec: "nodes.json",
                    classes: Some(&["TableFilterSet", "RowGroupOrderOptions", "StorageIndex"]),
                },
            },
        ],
    },
    Target {
        crate_dir: "crates/planner",
        crate_name: "rustdb_planner",
        derives: &["PartialEq"],
        modules: &[
            Module {
                name: "enums",
                content: Content::Enums(&[
                    "LogicalOperatorType",
                    "DistinctType",
                    "AggregateType",
                    "LimitNodeType",
                    "TupleDataValidityType",
                    "FileNameSegmentType",
                    "CopyOverwriteMode",
                    "CopyFunctionReturnType",
                    "PreserveOrderType",
                ]),
            },
            Module {
                name: "logical_operator",
                content: Content::Properties {
                    spec: "logical_operator.json",
                    classes: None,
                },
            },
            Module {
                name: "expression",
                content: Content::Properties {
                    spec: "expression.json",
                    classes: None,
                },
            },
            Module {
                name: "nodes",
                content: Content::Properties {
                    spec: "nodes.json",
                    classes: Some(&[
                        "BoundOrderByNode",
                        "BoundLimitNode",
                        "BoundCaseCheck",
                        "BoundPivotInfo",
                        "ColumnBinding",
                        "BoundParameterData",
                        "JoinCondition",
                        "ExtraOperatorInfo",
                    ]),
                },
            },
            Module {
                name: "result_modifier",
                content: Content::Properties {
                    spec: "result_modifier.json",
                    classes: Some(&["BoundOrderModifier"]),
                },
            },
        ],
    },
];

/// Rust variant names for C++ enum members whose mechanical CamelCase is awkward.
pub const ENUM_VARIANT_NAMES: &[(&str, &[(&str, &str)])] = &[(
    "LogicalTypeId",
    &[
        ("SQLNULL", "SqlNull"),
        ("TINYINT", "TinyInt"),
        ("SMALLINT", "SmallInt"),
        ("BIGINT", "BigInt"),
        ("HUGEINT", "HugeInt"),
        ("UTINYINT", "UTinyInt"),
        ("USMALLINT", "USmallInt"),
        ("UINTEGER", "UInteger"),
        ("UBIGINT", "UBigInt"),
        ("UHUGEINT", "UHugeInt"),
    ],
)];

/// Per-class overrides.
pub const CLASSES: &[ClassConfig] = &[
    // types.json
    ClassConfig {
        class: "ExtraTypeInfo",
        kind_name: Some("TypeInfo"),
        strip_suffix: Some("_TYPE_INFO"),
        null_variants: &["INVALID_TYPE_INFO"],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "InvalidTypeInfo",
        no_variant: true,
        deserialize_arm: Some("return Ok(None)"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "EnumTypeInfo",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "UnboundTypeInfo",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "interval_t",
        rust_name: Some("Interval"),
        derives: &["Copy", "Default"],
        repr_c: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ColumnIndex",
        data_type: Some("ColumnIndex"),
        fields: &[rename("type", "ty")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ExtensionTypeInfo",
        derives: &["Default"],
        ..ClassConfig::DEFAULT
    },
    // parsed_expression.json
    ClassConfig {
        class: "ParsedExpression",
        fields: &[rename("type", "expression_type")],
        extra_variants: &[ExtraVariant {
            variant: "Bound",
            ty: "crate::BoundExpression",
            discriminant: "BOUND_EXPRESSION",
            doc: "A sub-tree already bound by the binder (`BoundExpression`); binding only.",
            serialize: "Err(Error::serialization(\"Cannot serialize bound expression\"))",
        }],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BetweenExpression",
        fields: &[required("input"), required("lower"), required("upper")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CaseExpression",
        fields: &[required("else_expr")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CastExpression",
        fields: &[required("child")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CollateExpression",
        fields: &[required("child")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ComparisonExpression",
        fields: &[required("left"), required("right")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "FunctionExpression",
        fields: &[required("order_bys")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LambdaExpression",
        fields: &[required("lhs"), required("expr")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "StarExpression",
        custom: true,
        deserialize_arm: Some("crate::StarExpression::deserialize_kind(deserializer, &mut expression_type)?"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "SubqueryExpression",
        fields: &[required("subquery")],
        ..ClassConfig::DEFAULT
    },
    // query_node.json
    ClassConfig {
        class: "QueryNode",
        strip_suffix: Some("_NODE"),
        finalize_ported_in: Some("the CTE_NODE arm (`read_legacy_cte_node`)"),
        variant_names: &[("RECURSIVE_CTE_NODE", "RecursiveCte")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "SelectNode",
        fields: &[required("from_table")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "SetOperationNode",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "RecursiveCTENode",
        fields: &[required("left"), required("right")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CTENode",
        custom: true,
        no_variant: true,
        deserialize_arm: Some("return crate::query_node::read_legacy_cte_node(deserializer)"),
        ..ClassConfig::DEFAULT
    },
    // result_modifier.json
    ClassConfig {
        class: "ResultModifier",
        strip_suffix: Some("_MODIFIER"),
        ..ClassConfig::DEFAULT
    },
    // tableref.json
    ClassConfig {
        class: "TableRef",
        variant_names: &[("EMPTY_FROM", "Empty"), ("SHOW_REF", "Show")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "JoinRef",
        fields: &[required("left"), required("right")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "SubqueryRef",
        fields: &[required("subquery")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "TableFunctionRef",
        fields: &[required("function")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "PivotRef",
        fields: &[required("source")],
        extra_fields: &[ExtraField {
            name: "bound",
            ty: "Option<Box<crate::BoundPivotSpec>>",
            doc: "`bound_*`: set on a pivot the binder rewrote; never serialized.",
            default: "None",
        }],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "AtClause",
        fields: &[required("expr")],
        ..ClassConfig::DEFAULT
    },
    // statement.json
    ClassConfig {
        class: "SelectStatement",
        fields: &[required("node")],
        ..ClassConfig::DEFAULT
    },
    // nodes.json
    ClassConfig {
        class: "CommonTableExpressionInfo",
        fields: &[
            required("query"),
            FieldConfig {
                member: "materialized",
                serialize_with: Some(
                    "if serializer.should_serialize(7) { self.materialized } \
                     else { crate::CteMaterialize::CteMaterializeDefault }",
                ),
                ..FieldConfig::DEFAULT
            },
        ],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CommonTableExpressionMap",
        derives: &["Default"],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "OrderByNode",
        fields: &[rename("type", "order_type"), required("expression")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CaseCheck",
        fields: &[required("when_expr"), required("then_expr")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "SampleOptions",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "PivotColumn",
        fields: &[FieldConfig {
            member: "entries",
            serialize_with: Some("crate::tableref::pivot_entries_for_serialization(self, serializer)?"),
            ..FieldConfig::DEFAULT
        }],
        extra_fields: &[ExtraField {
            name: "subquery",
            ty: "Option<Box<crate::QueryNode>>",
            doc: "`subquery`: the query of `IN (SELECT ...)`, which the binder rejects.",
            default: "None",
        }],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ColumnDefinition",
        fields: &[rename("type", "logical_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ColumnList",
        derives: &["Default"],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "QualifiedColumnName",
        derives: &["Default", "Eq", "Hash"],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "VacuumOptions",
        derives: &["Default", "Copy", "Eq", "Hash"],
        ..ClassConfig::DEFAULT
    },
    // constraint.json
    ClassConfig {
        class: "CheckConstraint",
        fields: &[required("expression")],
        ..ClassConfig::DEFAULT
    },
    // create_info.json
    ClassConfig {
        class: "CreateInfo",
        strip_suffix: Some("_ENTRY"),
        extra_variants: &[ExtraVariant {
            variant: "Secret",
            ty: "Box<crate::CreateSecretInfo>",
            discriminant: "SECRET_ENTRY",
            doc: "`CreateSecretInfo` (`CatalogType::SECRET_ENTRY`); never serialized.",
            serialize: "Err(Error::serialization(\"Cannot serialize a CREATE SECRET\"))",
        }],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CreateTypeInfo",
        extra_fields: &[ExtraField {
            name: "query",
            ty: "Option<Box<crate::SelectStatement>>",
            doc: "`query`: the query of `CREATE TYPE ... AS ENUM (SELECT ...)`.",
            default: "None",
        }],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CreateViewInfo",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CreateMacroInfo",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    // parse_info.json
    ClassConfig {
        class: "ParseInfo",
        strip_suffix: Some("_INFO"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "AttachInfo",
        extra_fields: &[ExtraField {
            name: "parsed_options",
            ty: "rustdb_common::CaseInsensitiveMap<crate::ParsedExpression>",
            doc: "`parsed_options`: the options as written, before the binder evaluates them.",
            default: "rustdb_common::CaseInsensitiveMap::default()",
        }],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "CopyInfo",
        extra_fields: &[
            ExtraField {
                name: "parsed_options",
                ty: "rustdb_common::CaseInsensitiveMap<Option<crate::ParsedExpression>>",
                doc: "`parsed_options`: the options as written (`None` for a bare option name), before \
                      the binder evaluates them.",
                default: "rustdb_common::CaseInsensitiveMap::default()",
            },
            ExtraField {
                name: "file_path_expression",
                ty: "Option<Box<crate::ParsedExpression>>",
                doc: "`file_path_expression`: the file name when it is not a string constant.",
                default: "None",
            },
        ],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "DropInfo",
        fields: &[rename("type", "catalog_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "TransactionInfo",
        fields: &[rename("type", "transaction_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "VacuumInfo",
        fields: &[rename("ref", "table_ref")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "AddConstraintInfo",
        fields: &[required("constraint")],
        ..ClassConfig::DEFAULT
    },
    // macro_function.json
    ClassConfig {
        class: "MacroFunction",
        custom: true,
        strip_suffix: Some("_MACRO"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ScalarMacroFunction",
        fields: &[required("expression")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "TableMacroFunction",
        fields: &[required("query_node")],
        ..ClassConfig::DEFAULT
    },
    // extra_drop_info.json
    ClassConfig {
        class: "ExtraDropInfo",
        strip_suffix: Some("_INFO"),
        ..ClassConfig::DEFAULT
    },
    // dependency.json
    ClassConfig {
        class: "CatalogEntryInfo",
        fields: &[rename("type", "catalog_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalDependencyList",
        derives: &["Default"],
        ..ClassConfig::DEFAULT
    },
    // table_filter.json and nodes.json (property structs of the storage's table filters)
    ClassConfig {
        class: "TableFilter",
        data_type: Some("TableFilter"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ConstantFilter",
        // rustdb's filters compare with a `CompareOp`, written as its `ExpressionType`.
        fields: &[property_type(
            "comparison_type",
            "rustdb_types::compare::CompareOp",
        )],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ExpressionFilter",
        // The expression is the planner's, behind `table_filter::ExpressionFilter`.
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "TableFilterSet",
        fields: &[property_type(
            "filters",
            "&'a std::collections::BTreeMap<usize, crate::TableFilter>",
        )],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "RowGroupOrderOptions",
        // The order is the sort's (`rustdb_row::sort`), which has no serialization.
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "StorageIndex",
        data_type: Some("table::StorageIndex"),
        fields: &[rename("type", "ty")],
        ..ClassConfig::DEFAULT
    },
    // logical_operator.json (property structs of the planner's operators)
    ClassConfig {
        class: "LogicalOperator",
        data_type: Some("LogicalOperator"),
        fields: &[rename("type", "operator_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalSample",
        fields: &[required("sample_options")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalAnyJoin",
        fields: &[required("condition")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalInsert",
        fields: &[required("table_info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalDelete",
        fields: &[required("table_info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalUpdate",
        fields: &[required("table_info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalMergeInto",
        fields: &[required("table_info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundMergeIntoAction",
        data_type: Some("operator::BoundMergeIntoAction"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalCreateTable",
        fields: &[required("info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalCreate",
        fields: &[required("info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalSimple",
        fields: &[required("info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalCopyDatabase",
        fields: &[required("info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalCreateIndex",
        fields: &[required("info")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalExport",
        fields: &[required("copy_info"), required("exported_tables")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalGet",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalCopyToFile",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalExtensionOperator",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "LogicalVacuum",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "FileNameSegment",
        data_type: Some("operator::FileNameSegment"),
        fields: &[rename("type", "segment_type")],
        ..ClassConfig::DEFAULT
    },
    // expression.json (property structs of the planner's expressions)
    ClassConfig {
        class: "Expression",
        data_type: Some("Expression"),
        fields: &[rename("type", "expression_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundBetweenExpression",
        fields: &[required("input"), required("lower"), required("upper")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundCaseExpression",
        fields: &[required("else_expr")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundCastExpression",
        fields: &[required("child")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundComparisonExpression",
        fields: &[required("left"), required("right")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundLambdaExpression",
        fields: &[required("lambda_expr")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundParameterExpression",
        fields: &[required("parameter_data")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundUnnestExpression",
        fields: &[required("child")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundFunctionExpression",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundAggregateExpression",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundWindowExpression",
        custom: true,
        ..ClassConfig::DEFAULT
    },
    // nodes.json and result_modifier.json (property structs of the planner's nodes)
    ClassConfig {
        class: "BoundOrderByNode",
        data_type: Some("expression::BoundOrderByNode"),
        fields: &[rename("type", "order_type"), required("expression")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundLimitNode",
        data_type: Some("BoundLimitNode"),
        fields: &[rename("type", "limit_type")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundCaseCheck",
        data_type: Some("expression::BoundCaseCheck"),
        fields: &[required("when_expr"), required("then_expr")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundPivotInfo",
        data_type: Some("operator::BoundPivotInfo"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "ColumnBinding",
        data_type: Some("ColumnBinding"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundParameterData",
        data_type: Some("expression::BoundParameterData"),
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "JoinCondition",
        data_type: Some("operator::JoinCondition"),
        fields: &[required("left"), required("right")],
        ..ClassConfig::DEFAULT
    },
    ClassConfig {
        class: "BoundOrderModifier",
        data_type: Some("expression::BoundOrderModifier"),
        ..ClassConfig::DEFAULT
    },
];

/// The overrides for `class` (defaults when none are configured).
pub fn class_config(class: &str) -> &'static ClassConfig {
    CLASSES
        .iter()
        .find(|config| config.class == class)
        .unwrap_or(&ClassConfig::DEFAULT)
}
