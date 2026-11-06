//! C++ records cover native metadata, VARCHAR validation and a real CSV scan.
#[path = "../examples/csv_name_codec.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;

#[test]
fn native_csv_names_match_cpp() {
    let records = include_str!("data/native_csv_names_1.5.6.tsv");
    for line in records.lines().filter(|line| !line.starts_with("read\t")) {
        codec::compare(line, None).unwrap();
    }
}

#[test]
fn native_names_survive_file_binding_and_scanning() {
    // TMPDIR is set to the dedicated SSD artifact directory by the verification runner.
    let file = std::env::temp_dir().join(format!("native-csv-names-{}.csv", std::process::id()));
    std::fs::write(&file, b"42\n43\n").unwrap();
    let input = file.to_str().unwrap();
    for line in include_str!("data/native_csv_names_1.5.6.tsv")
        .lines()
        .filter(|line| line.starts_with("read\t"))
    {
        codec::compare(line, Some(input)).unwrap();
    }
    std::fs::remove_file(file).unwrap();
}
