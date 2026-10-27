# EXPORT logical-plan serialization parity

The 2026-10-27 completion audit confirms a supported public gap. Separate signed
C++ Python 1.5.6 and current source-8 Rust-wheel processes create identical tables,
a schema, primary/foreign keys, defaults, a view and an index. For both CSV and
Parquet, `json_serialize_plan('EXPORT DATABASE ...')` succeeds on C++ with
LOGICAL_EXPORT and returns NotImplemented on Rust. This is a functionality gap,
not an accepted performance exception. EXPLAIN controls and raw JSON are preserved.

Evidence is external `export-plan-parity-20261027/{before-2-index.json,
before-2-summary.json,cpp-before-2/report.json,rust-before-2/report.json}`. The first
probe fixture attempted a cross-schema foreign key that C++ itself rejects; it
failed before testing serialization and is retained in `cpp-before-1`. The corrected
fixture puts the child and parent in the same schema. No engine assertion was waived.

Frozen C++ serialization uses these numbered properties:

| Object | Properties |
|---|---|
| LogicalExport | Base logical operator, 200 copy_info, 201 exported_tables |
| BoundExportData | Base ParseInfo with BOUND_EXPORT_DATA type, 200 data |
| ExportedTableInfo | Required property 1 table_data |
| ExportedTableData | 1 table_name, 2 schema_name, 3 database_name, 4 file_path, 5 not_null_columns |

The Rust AST already generates BoundExportData/ExportedTableInfo/ExportedTableData
serialization. The logical export instead retains only runtime write metadata,
and its serializer explicitly returns NotImplemented. Preserve the actual bound
CopyInfo and exported-table data through binding; serialize the real objects with
the existing property codec. Match CSV default/readable options, catalog/schema
identity, file naming, physical/not-null columns, dependency order and the real
child COPY/UNION plan. Do not rewrite the output JSON to disguise binder differences.

Verify empty, one-, two- and multi-table exports; schema/view/default/constraint
metadata; CSV/Parquet options and encrypted Parquet; format/skip-default settings;
prepared/query behavior and actual export/import atomicity. Compare public plans
with independent C++ controls, preserving raw bytes and any strictly bounded fixture
path mapping. Check the numbered binary wire when introducing fields. Preserve
raw names and paths with ByteString; do not introduce lossy text conversion.

Then build/sign immutable current artifacts and run meaningful SDK/public/affected
SQL/client/persistence and final release gates. Source-8's full client and shell
checkpoint is a preserved before-stage control. No implementation or completion
is claimed by this before-fix contract.
