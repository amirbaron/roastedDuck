//! FIFO sizes describe queued bytes, not the total CSV input length.
#[cfg(unix)]
#[test]
fn fifo_is_not_seekable_and_all_buffers_are_read() {
    use rustdb_csv::buffer::{CsvBufferManager, CsvFileHandle};
    use rustdb_csv::options::CsvReaderOptions;
    use std::io::Write as _;
    let directory = std::env::temp_dir().join(format!("rustdb-csv-fifo-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("unique test directory");
    let path = directory.join("input.csv");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo")
            .success()
    );
    let data = format!("a|b|c\n{}", "1|2|3\n".repeat(100_000)).into_bytes();
    let expected = data.clone();
    let output = path.clone();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(output)
            .expect("FIFO writer");
        file.write_all(&data).expect("write all rows");
    });
    let options = CsvReaderOptions::default();
    let handle = CsvFileHandle::open(path.as_os_str().as_encoded_bytes(), &options).expect("open FIFO");
    let seekable = handle.can_seek();
    let manager = CsvBufferManager::with_handle(&options, path.as_os_str().as_encoded_bytes(), handle)
        .expect("buffer input");
    let mut actual = Vec::new();
    let mut index = 0;
    while let Some(buffer) = manager.get_buffer(index).expect("next buffer") {
        actual.extend_from_slice(buffer.data());
        index += 1;
    }
    writer.join().expect("writer completes");
    assert!(!seekable);
    assert_eq!(actual, expected);
    std::fs::remove_file(path).expect("remove own FIFO");
    std::fs::remove_dir(directory).expect("remove own directory");
}
