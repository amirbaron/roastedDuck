# ICU acceptance controls

`upstream.py` inventories every pinned engine SQL file with `require icu`, verifies
its hash against the source manifest, and executes the unchanged files on both
engines with one worker and normal engine thread defaults. The current inventory
has63 files; there are no extension-local SQL files under `extension/icu`.

Required arguments: `--slt`, `--shim`, `--source`, `--manifest`, `--extensions`,
`--dataset-extensions`, `--out`. Use the signed current SLT binary, pinned C++ shim,
DuckDB1.5.6 source and signed oracle packages. Keep outputs on the artifact volume.

LOCAL_EXTENSION_REPO changes the entire SLT runner to autoload mode. The script
sets it only for files that declare that prerequisite. Other originals keep their
normal loading mode, including their `no_extension_autoloading` requirements.
The owned local repository contains version/platform paths and signed packages;
JSON/ICU packages are needed for the autoloading-types original. No SQL, expected
result, prerequisite or mode statement is rewritten.

`sort_key_control.py --cli ... --oracle ... --out ...` compares exact sort-key
output across the oracle's ICU locale inventory plus keyword/BCP47 forms. The
bounded Unicode fixture contains normalization variants, combining marks, scripts,
emoji, NUL and NULL. It records raw CSV and hashes, and requires both successful
execution and the complete expected row count before comparing bytes.

`scripts/capi-tests/collation-controls.py` separately checks public C API values,
metadata and errors. ICU crate unit tests cover the runtime/calendar helpers.
These checks qualify the selected source/images; they are not package publication
or a claim that all historical stress/performance collections were rerun.
