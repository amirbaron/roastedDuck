//! String results must survive scanner resets, buffer release, and vector slicing.

use std::sync::Arc;

use rustdb_csv::{
    multi_file::{FileList, MultiFileOptions},
    options::CsvReaderOptions,
    read_csv::{bind_csv_files, read_csv_functions},
};
use rustdb_function::{TableScanState, cast::CastFunctionSet};
use rustdb_types::{DataChunk, LogicalType, SelectionVector};

#[test]
fn retained_strings_outlive_csv_scan_and_file() {
    let root = std::env::temp_dir().join(format!("csv-string-lifetime-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("input.csv");
    let expected: Vec<String> = (0..5000)
        .map(|row| {
            let suffix = "x".repeat(if row % 127 == 0 { 11000 } else { 101 });
            if row % 3 == 0 {
                format!("row{row}-雪,\"quoted\"\n{suffix}")
            } else {
                format!("row{row}-雪-{suffix}")
            }
        })
        .collect();
    let mut csv = String::new();
    for value in &expected {
        if value.contains([',', '"', '\n']) {
            csv.push('"');
            csv.push_str(&value.replace('"', "\"\""));
            csv.push('"');
        } else {
            csv.push_str(value);
        }
        csv.push('\n');
    }
    std::fs::write(&path, csv).unwrap();
    let mut options = CsvReaderOptions {
        auto_detect: false,
        columns_set: true,
        parallel: false,
        name_list: vec!["value".into()],
        sql_type_list: vec![LogicalType::VARCHAR],
        ..CsvReaderOptions::default()
    };
    options.dialect_options.header.set(false, true);
    options.maximum_line_size.set(32768, true);
    options.buffer_size_option.set(65536, true);
    let data = bind_csv_files(
        &Arc::new(CastFunctionSet::new()),
        FileList {
            files: vec![path.as_os_str().as_encoded_bytes().into()],
        },
        MultiFileOptions::default(),
        options,
        None,
    )
    .unwrap();
    let functions = read_csv_functions();
    let scan = functions[0].functions[0].scan.as_ref().unwrap();
    let mut state = TableScanState::default();
    let mut chunk = DataChunk::default();
    let mut retained = Vec::new();
    loop {
        let more = scan(Some(&data), &[0], &mut state, &mut chunk).unwrap();
        if !chunk.is_empty() {
            let selection = SelectionVector::new((0..chunk.len() as u32).rev().collect::<Vec<_>>());
            retained.push((chunk.column(0).slice(&selection), chunk.len()));
        }
        if !more {
            break;
        }
    }
    drop(chunk);
    drop(state);
    drop(data);
    std::fs::remove_dir_all(root).unwrap();

    let mut offset = 0;
    for (vector, count) in retained {
        let cloned = vector.clone();
        drop(vector);
        for row in 0..count {
            assert_eq!(
                cloned.string_bytes(row),
                expected[offset + count - row - 1].as_bytes()
            );
        }
        offset += count;
    }
    assert_eq!(offset, expected.len());
}
