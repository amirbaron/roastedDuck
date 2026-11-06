//! Frozen C++ 1.5.6 file-handle log messages and profiler MAP-value boundary.
use std::any::Any;
use std::sync::{Arc, Mutex};

use rustdb_common::{ByteString, Result};
use rustdb_function::FunctionClient;
use rustdb_function::logging::{ClientLogger, FileSystemLogger, LogLevel, LogScope, LogTypeInfo};
use rustdb_types::{LogicalType, Value};

#[derive(Debug, Default)]
struct Capture(Mutex<Vec<ByteString>>);
impl ClientLogger for Capture {
    fn should_log(&self, _: LogScope, name: &str, _: LogLevel) -> bool {
        name == "FileSystem"
    }
    fn write_log_bytes(&self, _: LogScope, _: &str, _: LogLevel, message: &[u8]) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.into());
        Ok(())
    }
    fn lookup_log_type(&self, _: &str) -> Option<LogTypeInfo> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn unhex(text: &str) -> Result<Vec<u8>> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(rustdb_common::Error::invalid_input("odd oracle hex length"));
    }
    pairs
        .iter()
        .map(|pair| {
            let digit = |byte: u8| -> Result<u8> {
                match byte {
                    b'0'..=b'9' => Ok(byte - b'0'),
                    b'a'..=b'f' => Ok(byte - b'a' + 10),
                    _ => Err(rustdb_common::Error::invalid_input("invalid oracle hex digit")),
                }
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}
#[test]
fn native_file_handle_messages_and_checked_profile_map_match_cpp() {
    let input = std::env::var_os("RUSTDB_FILE_LOGGING_ORACLE").map_or_else(
        || include_str!("data/native_file_logging_1.5.6.tsv").to_owned(),
        |path| std::fs::read_to_string(path).unwrap(),
    );
    let client = FunctionClient::default();
    let capture = Arc::new(Capture::default());
    client.set_logger(capture.clone());
    let mut count = 0;
    for line in input.lines().filter(|line| !line.is_empty()) {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), 3);
        let mode: usize = fields[0].parse().unwrap();
        let name = unhex(fields[1]).unwrap();
        let result: Result<ByteString> = if mode == 4 {
            (|| {
                let checked = Value::try_varchar(&name)?;
                Value::map(
                    LogicalType::VARCHAR,
                    LogicalType::VARCHAR,
                    vec![(checked.clone(), checked)],
                )
                .to_varchar_bytes()
            })()
        } else {
            capture.0.lock().unwrap().clear();
            let logger = FileSystemLogger::open_for(Some(&client), &name).unwrap();
            logger.read(42, 7);
            logger.write(19, 3);
            logger.close();
            Ok(capture.0.lock().unwrap()[mode].clone())
        };
        let (status, expected) = fields[2].split_at(3 + usize::from(fields[2].starts_with("ERR")));
        let expected = unhex(expected).unwrap();
        match result {
            Ok(value) => {
                assert_eq!(status, "OK:");
                assert_eq!(value.as_bytes(), expected, "mode {mode}, input hex {}", fields[1]);
            }
            Err(error) => {
                assert_eq!(status, "ERR:");
                assert_eq!(
                    error.raw_message_bytes(),
                    expected,
                    "mode {mode}, input hex {}",
                    fields[1]
                );
            }
        }
        count += 1;
    }
    assert!(count >= 60);
}
