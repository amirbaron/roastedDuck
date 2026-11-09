//! Native path/hive contracts and real CSV/BLOB journeys on the SSD.
#[path = "../examples/support/native_hive.rs"]
mod hive;
use roastedduck_common::{ByteString, CaseInsensitiveMap};
use roastedduck_csv::{
    multi_file::{FileList, MultiFileOptions, create_file_list},
    options::CsvReaderOptions,
    read_csv::{bind_csv_files, read_csv_functions},
};
use roastedduck_function::{TableScanState, cast::CastFunctionSet, copy::CopyFunctionBindInput};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::sync::Arc;
#[test]
fn hive_native_cpp_fixture() {
    for line in include_str!("data/native_hive_1.5.6.tsv").lines() {
        hive::compare(line).unwrap();
    }
}
#[test]
fn unicode_csv_file_list_scan_and_native_blob_copy() {
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("csv-native-journey-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let dir = root.join("界");
    std::fs::create_dir(&dir).unwrap();
    let csv = dir.join("雪.csv");
    std::fs::write(&csv, b"42\n7\n").unwrap();
    let pattern = roastedduck_common::byte_format!("{}/*.csv", dir.as_os_str().as_encoded_bytes());
    let files = create_file_list("read_csv", &[pattern], "csv").unwrap();
    assert_eq!(
        files.files,
        vec![ByteString::from(csv.as_os_str().as_encoded_bytes())]
    );
    let mut options = CsvReaderOptions {
        auto_detect: false,
        columns_set: true,
        parallel: false,
        name_list: vec!["値".into()],
        sql_type_list: vec![LogicalType::INTEGER],
        ..CsvReaderOptions::default()
    };
    options.dialect_options.header.set(false, true);
    let data = bind_csv_files(
        &Arc::new(CastFunctionSet::new()),
        FileList { files: files.files },
        MultiFileOptions::default(),
        options,
        None,
    )
    .unwrap();
    let functions = read_csv_functions();
    let scan = functions[0].functions[0].scan.as_ref().unwrap();
    let mut state = TableScanState::default();
    let mut chunk = DataChunk::default();
    let mut values = Vec::new();
    loop {
        let more = scan(Some(&data), &[0], &mut state, &mut chunk).unwrap();
        for row in 0..chunk.len() {
            values.push(chunk.column(0).get_value(row).as_i32().unwrap());
        }
        if !more {
            break;
        }
    }
    assert_eq!(values, vec![42, 7]);
    assert_eq!(data.names, vec![ByteString::from("値")]);
    let output = root.join("blob-出力.bin");
    let path = output.as_os_str().as_encoded_bytes();
    let function = roastedduck_csv::copy_blob::blob_copy_function();
    let opts = CaseInsensitiveMap::default();
    let mut input = CopyFunctionBindInput {
        options: &opts,
        file_path: path,
        format: b"blob",
        file_extension: "blob".into(),
        binder: None,
    };
    let bind =
        function.copy_to_bind.as_ref().unwrap()(&mut input, &["bytes".into()], &[LogicalType::BLOB]).unwrap();
    let global = function.copy_to_initialize_global.as_ref().unwrap()(bind.as_ref(), path).unwrap();
    let mut vector = Vector::flat(LogicalType::BLOB, 2);
    vector.set_value(0, &Value::blob(b"a\0b")).unwrap();
    vector.set_value(1, &Value::blob(b"\xff")).unwrap();
    let block = DataChunk::new(vec![vector], 2);
    function.copy_to_sink.as_ref().unwrap()(bind.as_ref(), &global, &block).unwrap();
    function.copy_to_finalize.as_ref().unwrap()(bind.as_ref(), &global).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"a\0b\xff");
    drop(global);
    drop(data);
    std::fs::remove_dir_all(&root).unwrap();
}
