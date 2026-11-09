//! Real physical COPY pipeline with Unicode paths, temp rename and returned filenames.
use roastedduck_common::Error;
use roastedduck_execution::operators::{
    CollectedResult, CopyToFileOptions, PhysicalCopyToFile, PhysicalResultCollector,
};
use roastedduck_execution::{ClientEnv, Executor, PhysicalRef, create_physical_plan};
use roastedduck_function::{CopyFunction, FunctionData};
use roastedduck_planner::operator::{CopyFunctionReturnType, CopyOverwriteMode, FilenamePattern};
use roastedduck_planner::{Expression, LogicalKind, LogicalOperator};
use roastedduck_types::{LogicalType, Value};
use std::any::Any;
use std::io::Write;
use std::sync::{Arc, Mutex};
#[derive(Debug)]
struct Bind;
impl FunctionData for Bind {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn function() -> CopyFunction {
    let mut f = CopyFunction::new("native_test_blob");
    f.copy_to_initialize_global = Some(Arc::new(|_, path| {
        let file = std::fs::File::create(roastedduck_function::file_system::native_path(path)?)
            .map_err(|e| Error::invalid_input(e.to_string()))?;
        Ok(Box::new(Mutex::new(file)))
    }));
    f.copy_to_sink = Some(Arc::new(|_, state, chunk| {
        let file = state
            .downcast_ref::<Mutex<std::fs::File>>()
            .ok_or_else(|| Error::internal("missing test writer"))?;
        let mut file = file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for row in 0..chunk.len() {
            let value = chunk.column(0).get_value(row);
            file.write_all(value.as_bytes().unwrap_or_default())
                .map_err(|e| Error::invalid_input(e.to_string()))?;
        }
        Ok(())
    }));
    f.copy_to_finalize = Some(Arc::new(|_, state| {
        state
            .downcast_ref::<Mutex<std::fs::File>>()
            .ok_or_else(|| Error::internal("missing test writer"))?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flush()
            .map_err(|e| Error::invalid_input(e.to_string()))
    }));
    f
}
#[test]
fn physical_copy_writes_renames_and_returns_native_path() {
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("physical-native-copy-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let target = root.join("出力.bin");
    let tmp = root.join("tmp_出力.bin");
    let scan = LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 });
    let mut plan = LogicalOperator::projection(1, vec![Expression::constant(Value::blob(b"a\0b\xff"))], scan);
    plan.resolve_operator_types().unwrap();
    let input = create_physical_plan(plan).unwrap();
    let copy: PhysicalRef = Arc::new(PhysicalCopyToFile::new(
        input,
        function(),
        Arc::new(Bind),
        CopyToFileOptions {
            file_path: tmp.as_os_str().as_encoded_bytes().into(),
            use_tmp_file: true,
            overwrite_mode: CopyOverwriteMode::ErrorOnConflict,
            filename_pattern: FilenamePattern::default(),
            file_extension: "bin".into(),
            per_thread_output: false,
            file_size_bytes: None,
            rotate: false,
            partition_output: false,
            partition_columns: Vec::new(),
            write_partition_columns: false,
            write_empty_file: true,
            hive_file_pattern: true,
            return_type: CopyFunctionReturnType::ChangedRowsAndFileList,
            names: vec!["bytes".into()],
            expected_types: vec![LogicalType::BLOB],
        },
    ));
    let root_op: PhysicalRef = Arc::new(PhysicalResultCollector::new(copy));
    let state = Executor::execute(&root_op, ClientEnv::standalone().unwrap()).unwrap();
    let rows = state
        .downcast_ref::<Mutex<CollectedResult>>()
        .unwrap()
        .lock()
        .unwrap();
    let chunk = &rows.chunks[0];
    assert_eq!(chunk.column(0).get_value(0).as_i128(), Some(1));
    let files = chunk.column(1).get_value(0);
    assert_eq!(
        files.children().unwrap()[0].as_bytes(),
        Some(target.as_os_str().as_encoded_bytes())
    );
    assert!(!tmp.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"a\0b\xff");
    drop(rows);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn physical_export_writes_native_sql_files() {
    use roastedduck_execution::operators::PhysicalExport;
    use roastedduck_planner::operator::{ExportedTable, LogicalExport};
    let directory = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("physical-native-export-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let raw_path = roastedduck_common::ByteString::from(b"raw\xff'\\file.csv");
    let export = LogicalExport {
        copy_info: roastedduck_ast::CopyInfo {
            catalog: "".into(),
            schema: "main".into(),
            table: "".into(),
            select_list: Vec::new(),
            is_from: false,
            format: "csv".into(),
            file_path: directory.as_os_str().as_encoded_bytes().into(),
            options: roastedduck_common::CaseInsensitiveMap::default(),
            select_statement: None,
            is_format_auto_detected: false,
            parsed_options: roastedduck_common::CaseInsensitiveMap::default(),
            file_path_expression: None,
        },
        function: CopyFunction::new("csv"),
        file_path: directory.as_os_str().as_encoded_bytes().into(),
        format: "csv".into(),
        options: Vec::new(),
        catalog: "memory".into(),
        tables: vec![ExportedTable {
            database_name: "".into(),
            schema_name: "main".into(),
            table_name: b"t\xff".into(),
            file_path: raw_path,
            not_null_columns: Vec::new(),
        }],
    };
    let operator: PhysicalRef = Arc::new(PhysicalExport::new(
        None,
        export,
        Arc::new(roastedduck_catalog::Catalog::new("memory").unwrap()),
        Arc::new(roastedduck_catalog::Transaction::system()),
    ));
    let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(operator));
    Executor::execute(&root, ClientEnv::standalone().unwrap()).unwrap();
    assert_eq!(std::fs::read(directory.join("schema.sql")).unwrap(), b"\n");
    assert_eq!(
        std::fs::read(directory.join("load.sql")).unwrap(),
        b"COPY \"t\xff\" FROM 'raw\xff''/file.csv' (FORMAT 'csv');\n"
    );
    std::fs::remove_dir_all(directory).unwrap();
}
