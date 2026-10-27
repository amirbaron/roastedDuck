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

## Signed SDK checkpoint and registered-backend counterexample

The first immutable signed release SDK passes all three plan/execution
regressions in 0.04 seconds with all 912 inputs/signatures unchanged. This covers
empty/default metadata, a four-child logical UNION, and actual CSV/Parquet
export/import preserving defaults, foreign keys and generated columns.
Evidence: `sdk-verification-1.json`. Public full-plan equality remains pending.

A separate signed C++/before-stage Rust-wheel probe confirms a co-located
runtime bug: C++ EXPORT to a registered memory filesystem creates the data file,
schema.sql and load.sql and imports them successfully for CSV/Parquet with new
and preexisting directories. Rust reports EXPORT success with only the data
file in the registered backend; its metadata is written under a local memory:
directory instead, so import cannot find schema.sql. The isolated probe cwd is
on the external SSD, and these misrouted files remain evidence. This is not a
shared C++ limit. Evidence: `memory-before-1-index.json`,
`{cpp,rust}-memory-before-1/report.json`.

Route directory existence/creation and both metadata writes through the
registered filesystem when one handles the path, preserving native paths
otherwise. Use existing opener/access-policy and streamed-output helpers, and
retain backend error type/metadata through failures. Add actual registered
CSV/Parquet export/import and empty-export metadata-error regressions using
the existing test backend; do not disguise the URI as a local path. Reverify
component and public installed-package behavior before closure.

## Registered routing implementation and retained SDK fixture correction

Directory handling now dispatches to registered is_dir/mkdir, and metadata writes
use the registered output stream with backend errors retained. Local writes keep
the existing native-path/error rendering. The test target reuses nine registered
storage/WAL cases and adds registered CSV/Parquet export/import plus empty-export
metadata failure/recovery. Source6 workspace/all-target Clippy passes. Its signed
SDK2 run passes13/14: the success-path assertion incorrectly expected unquoted
sample, a reserved keyword. Frozen C++ memory-before1 records quote this name in
both schema.sql and load.sql, as does Rust. Correct those two literal assertions;
do not relax import/data/constraint/default checks. The backend error and all nine
storage/WAL tests passed. Preserve sdk-verification-2.json and sdk-run-2.log as
failed fixture evidence, not an engine pass.

The first full C ABI plan probe also lacked its parent export directory; all128
C++ cases returned IO errors before reaching serialization. cpp-public-1/report.json
is retained. The corrected probe creates one absolute shared fixture parent and
compares every plan field for both engines without JSON/path rewriting. These
component corrections require new snapshots/builds; no final artifact or public
plan parity is claimed yet.

## Registered routing signed SDK pass

Source7 passes workspace/all-target Clippy6 and all14 tests in immutable signed
release SDK3 (0.13seconds), with all912 build inputs and the signed image unchanged.
The corrected reserved-name literals now match the frozen C++ schema/load output.
CSV/Parquet backend data and metadata round-trip into a new registered connection,
including Unicode/NULL rows, primary-key enforcement and defaults. Empty EXPORT
preserves callback error kind/subtype/native extra metadata and permits subsequent
successful export/query. The nine existing backend checkpoint/WAL/access tests also
pass. The SDK's isolated external cwd remains empty, so no URI directory was created
locally. Evidence: sdk-verification-3.json, sdk-run-3.log, source-7/manifest.json.

The C++ public3 probe records128 successful complete plans with no JSON errors.
public2 retained64 parser errors from an incorrect explicit-catalog fixture; the
frozen grammar requires EXPORT DATABASE memory TO 'path'. The corrected probe uses
that literal grammar. The C API library is rebuilding from source7 before Rust plan
comparison; no public equality or whole-port completion is asserted yet.

## First signed C API comparison and next binder correction

The source7 C API-only release build exited0, then was copied/signed/strictly verified
as capi-signed-1/libduckdb.dylib (SHA256
919ba59b2eb882d15ef6284162d6fa77135d8855b58dd80a4e0f25619191fa19), retaining
547 shipping exports/zero ADBC test hooks. All912 build inputs remain unchanged.
Both engines serialize all128 public cases successfully. Full comparison finds96
mismatching cases, exclusively root CopyInfo option-array iteration order; every
other field, child COPY, UNION, bound table, default and explicit catalog matches.
No key/order is filtered from comparison. Evidence: public-comparison-3.json and
{cpp,rust}-public-3/report.json.

The binder currently appends evaluated/default options to the ordered transport
container, whereas frozen C++ inserts into case_insensitive_map_t. Preserve that
actual map lifecycle with the existing libc++ CIHash/order model, including erased
write-only options and CSV defaults; store the resulting bound map for both real
load.sql and plan serialization. Do not sort/reverse JSON arrays to conceal it.
Add literal frozen option-order assertions and reverify signed components/public
plans. No complete EXPORT plan parity is claimed by the current mismatch.

Separately runtime-1 passes all eight CSV/Parquet export/import journeys in both
engine directions, four exports per writer (default/explicit catalog). Three-table
exports preserve view/index/generated column/default/Unicode/NULL rows and both
primary/foreign-key enforcement after the opposite engine imports. This validates
the signed image above, not a future binder correction. Keep the reports/files
and preserve the source7 control before rebuilding.

## Bound option-map correction component pass

The binder now replays evaluated options/default insertion with existing libc++
CIHash/bucket-order modeling before write-only erasure, then retains that actual
ordered bound map for metadata generation and serialization. No JSON rewriting is
used. Five signed lightweight model fixtures match literal C++ root option order,
including a larger CSV map with bucket growth and force_quote removal. C++ public5
records160 successful plans. The earlier public4 fixture included ENCODING, which
C++ permits only for reading; its24 writer-binding errors are preserved. The valid
fixture removes that unsupported option and retains all other coverage.

Source8 has913 overlay entries/912 build inputs. Workspace/all-target Clippy7 passes,
and immutable signed SDK4 passes all14 tests in0.12seconds, including the three
new literal C++ option-order assertions and the registered storage/WAL/export cases.
Evidence: option-order-verification-1.json, sdk-verification-4.json,
source-8/manifest.json, cpp-public-5/report.json. The C API-only release rebuild is
active for full public equality and refreshed cross-engine runtime checks. These
component passes do not close final whole-port gates.
