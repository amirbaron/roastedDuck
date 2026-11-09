//! Native cache keys and Unicode-path Parquet write/read/reopen journeys.
use roastedduck_common::ByteString;
use roastedduck_function::ObjectCache;
use roastedduck_parquet::{
    reader::{CachedMetadata, ParquetReader, ScanColumn},
    schema::ParquetOptions,
    writer::{ParquetWriter, WriterOptions},
};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::sync::Arc;
#[test]
fn object_cache_distinguishes_every_native_byte_key() {
    let cache = ObjectCache::default();
    for byte in 0..=255u8 {
        let key = [b'p', byte];
        cache.put(ByteString::from(key.as_slice()), Arc::new(u16::from(byte)));
    }
    for byte in 0..=255u8 {
        assert_eq!(*cache.get::<u16>([b'p', byte]).unwrap(), u16::from(byte));
    }
    assert!(cache.get::<u16>(b"p\xef\xbf\xbd").is_none());
    assert_ne!(CachedMetadata::key(b"x\xff"), CachedMetadata::key(b"x\x80"));
}
#[test]
fn parquet_unicode_path_preserves_data_and_reuses_metadata() {
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("parquet-native-journey-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("雪-界.parquet");
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut column = Vector::flat(LogicalType::INTEGER, 2);
    column.set_value(0, &Value::integer(42)).unwrap();
    column.set_value(1, &Value::integer(7)).unwrap();
    let chunk = DataChunk::new(vec![column], 2);
    let mut writer = ParquetWriter::new(
        bytes,
        &["値".into()],
        &[LogicalType::INTEGER],
        WriterOptions::default(),
    )
    .unwrap();
    writer.flush(&[chunk]).unwrap();
    writer.finalize().unwrap();
    drop(writer);
    let cache = Arc::new(ObjectCache::default());
    let options = ParquetOptions {
        metadata_cache: Some(cache),
        ..ParquetOptions::default()
    };
    let first = ParquetReader::open(bytes, options.clone()).unwrap();
    let second = ParquetReader::open(bytes, options).unwrap();
    assert_eq!(first.file_name(), bytes);
    assert_eq!(first.columns[0].name, "値");
    assert!(Arc::ptr_eq(&first.context.metadata, &second.context.metadata));
    let mut scan = second
        .initialize_scan(vec![0], &[ScanColumn::column(0)], Vec::new())
        .unwrap();
    let mut vectors = Vec::new();
    let mut values = Vec::new();
    loop {
        let count = scan.scan(&mut vectors).unwrap();
        if count == 0 {
            break;
        }
        for row in 0..count {
            values.push(vectors[0].get_value(row).as_i32().unwrap());
        }
    }
    assert_eq!(values, vec![42, 7]);
    drop(scan);
    drop(second);
    drop(first);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_open_errors_match_cpp_bytes_and_errno() {
    use roastedduck_function::{
        caching_file_system::CachingFileHandle, external_file_cache::CacheValidationMode,
    };
    fn decode(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }
    for line in include_str!("data/native_open_1.5.6.tsv").lines() {
        let (path, expected) = line.split_once('\t').unwrap();
        let error =
            CachingFileHandle::open(None, decode(path), CacheValidationMode::ValidateAll).unwrap_err();
        assert_eq!(
            error.into_json().to_bytes(),
            decode(expected.strip_prefix("ERR:").unwrap())
        );
    }
}

#[test]
#[cfg(unix)]
fn native_open_uses_c_string_os_boundary_and_keeps_cache_key() {
    use roastedduck_function::{
        caching_file_system::CachingFileHandle, external_file_cache::CacheValidationMode,
    };
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("external test TMPDIR"))
        .join(format!("parquet-native-open-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("雪.bin");
    std::fs::write(&path, b"a\0b\xff").unwrap();
    let mut native = path.as_os_str().as_encoded_bytes().to_vec();
    native.extend_from_slice(b"\0unused\xff");
    let handle = CachingFileHandle::open(None, &native, CacheValidationMode::ValidateAll).unwrap();
    assert_eq!(handle.path(), native);
    assert_eq!(handle.read(4, 0).unwrap().bytes(), b"a\0b\xff");
    drop(handle);
    CachingFileHandle::open(
        None,
        root.as_os_str().as_encoded_bytes(),
        CacheValidationMode::ValidateAll,
    )
    .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
