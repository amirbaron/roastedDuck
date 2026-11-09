//! Reads must stop at the original file size even if the CSV grows after opening.

#[test]
fn original_size_boundary_retains_final_eof_read() {
    use roastedduck_csv::buffer::CsvFileHandle;
    use roastedduck_csv::options::CsvReaderOptions;
    use std::io::Write as _;

    let directory = std::env::temp_dir().join(format!("roastedduck-csv-boundary-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("unique test directory");
    let path = directory.join("input.csv");
    std::fs::write(&path, b"a\n1\n").expect("initial CSV");
    let mut handle = CsvFileHandle::open(path.as_os_str().as_encoded_bytes(), &CsvReaderOptions::default())
        .expect("open original CSV");
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append handle");
    writer.write_all(b"2\n").expect("grow CSV after opening");
    drop(writer);

    let mut buffer = [0; 128];
    let count = handle.read(&mut buffer).expect("original rows");
    assert_eq!(&buffer[..count], b"a\n1\n");
    assert!(!handle.finished_reading());
    assert_eq!(handle.read(&mut buffer).expect("bounded EOF read"), 0);
    assert!(handle.finished_reading());
    drop(handle);
    std::fs::remove_file(path).expect("remove own CSV");
    std::fs::remove_dir(directory).expect("remove own directory");
}
