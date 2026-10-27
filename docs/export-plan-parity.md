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

## First implementation component stage

The working logical export now retains its actual bound CopyInfo. Runtime
exported-table records use the generated ExportedTableData type, extended in
the code generator to own ByteString names/paths and vectors of native column
names. The serializer uses the existing ParseInfo/BoundExportData/property
codecs. Requested/default catalog identity remains in serialized metadata;
runtime lookup still uses the resolved catalog. Child COPY records preserve
their real default auto-format flag. Multiple table copies now form one logical
UNION with all children; physical planning already lowers that to binary unions.

A signed generator check confirms all generated code matches its configuration.
An independently compiled/signed native C++ oracle records seven exact
ExportedTableData binary wires: empty/plain names, invalid UTF-8, embedded NUL,
all byte values and a long name. The signed Rust wire regression decodes and
re-encodes every wire byte-identically. Workspace/all-target check and Clippy
with warnings denied pass after retained boxed-value/style corrections.

Source 4 has 913 overlay entries/912 verified build inputs. The release SDK build is
active for three regressions: empty metadata/default catalog, four-table UNION
and bound metadata, and actual multi-table CSV/Parquet export/import with
foreign keys/default/generated columns. Runtime SDK/public plan equality,
prepared/options/raw-name edge cases and broader final gates remain pending.
No complete EXPORT or whole-port parity is claimed by these component checks.

Evidence: external `export-plan-parity-20261027/{source-4/manifest.json,
codegen-check-1-terminal.json,cpp-wire-1/verification.json,wire-verification-1.json,
check-build-2-terminal.json,clippy-build-3-terminal.json,sdk-build-1-manifest.json}`.
Earlier failed check/style snapshots remain separate.
