//! Native filesystem operations and real access-control paths on Unix.
#[path = "../examples/support/native_paths.rs"]
mod codec;
use rustdb_common::{Error, Result};
use rustdb_function::{FunctionClient, SettingInfo, SettingScope, SettingsSource};
use rustdb_types::{LogicalType, Value};
use std::sync::Arc;
#[derive(Debug)]
struct AccessSettings {
    allowed: Vec<u8>,
    home: Vec<u8>,
}
impl SettingsSource for AccessSettings {
    fn current_setting(&self, name: &str) -> Result<Value> {
        Ok(match name {
            "enable_external_access" => Value::boolean(false),
            "allowed_paths" => Value::list(LogicalType::VARCHAR, Vec::new()),
            "allowed_directories" => Value::list(
                LogicalType::VARCHAR,
                vec![Value::bytes(LogicalType::VARCHAR, &self.allowed)],
            ),
            "home_directory" => Value::bytes(LogicalType::VARCHAR, &self.home),
            _ => return Err(Error::invalid_input("unknown test setting")),
        })
    }
    fn current_setting_scope(&self, _: &str) -> Option<(Value, SettingScope)> {
        None
    }
    fn settings(&self) -> Vec<SettingInfo> {
        Vec::new()
    }
}
#[test]
fn native_path_oracle_subset_matches_cpp() {
    for line in include_str!("data/native_paths_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
#[cfg(unix)]
#[test]
fn raw_paths_preserve_os_names_and_enforce_allowed_directories() {
    use rustdb_function::file_system::{
        activate_opener, expand_path_bytes, native_path, verify_can_access_directory, verify_can_access_file,
    };
    use std::os::unix::fs::symlink;
    let root = std::path::PathBuf::from(std::env::var_os("TMPDIR").expect("SSD test TMPDIR"))
        .join(format!("native-path-permissions-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let root_bytes = root.as_os_str().as_encoded_bytes();
    let allowed = rustdb_common::byte_format!("{root_bytes}/allowed-{}", "é".as_bytes());
    let outside = rustdb_common::byte_format!("{root_bytes}/outside-{}", "界".as_bytes());
    let sibling = rustdb_common::byte_format!("{allowed}-sibling");
    for path in [&allowed, &outside, &sibling] {
        std::fs::create_dir(native_path(path).unwrap()).unwrap();
    }
    let file = rustdb_common::byte_format!("{allowed}/data-{}", "雪".as_bytes());
    std::fs::write(native_path(&file).unwrap(), b"native path contents").unwrap();
    assert_eq!(
        std::fs::read(native_path(&file).unwrap()).unwrap(),
        b"native path contents"
    );
    let raw = rustdb_common::byte_format!("{allowed}/raw-{}", b"\xff".as_slice());
    let raw_created = std::fs::create_dir(native_path(&raw).unwrap());
    if let Err(error) = &raw_created {
        assert_eq!(error.raw_os_error(), Some(92), "unexpected raw path OS failure");
    }
    let client = Arc::new(FunctionClient::default());
    client.set_settings(Arc::new(AccessSettings {
        allowed: allowed.as_bytes().to_vec(),
        home: allowed.as_bytes().to_vec(),
    }));
    let _opener = activate_opener(client);
    assert_eq!(
        expand_path_bytes(b"~/x\xff").as_bytes(),
        rustdb_common::byte_format!("{allowed}/x{}", b"\xff".as_slice()).as_bytes()
    );
    assert!(verify_can_access_file(&file).is_ok());
    assert!(verify_can_access_directory(&allowed).is_ok());
    assert!(
        verify_can_access_file(rustdb_common::byte_format!(
            "{allowed}/raw-{}",
            b"\xff".as_slice()
        ))
        .is_ok()
    );
    let denied = verify_can_access_file(rustdb_common::byte_format!(
        "{outside}/raw-{}",
        b"\xff".as_slice()
    ))
    .unwrap_err();
    assert!(denied.raw_message_bytes().contains(&0xff));

    assert!(verify_can_access_file(rustdb_common::byte_format!("{sibling}/file")).is_err());
    assert!(
        verify_can_access_file(rustdb_common::byte_format!(
            "{allowed}/../outside-{}/file",
            "界".as_bytes()
        ))
        .is_err()
    );
    let link = rustdb_common::byte_format!("{allowed}/escape");
    symlink(native_path(&outside).unwrap(), native_path(&link).unwrap()).unwrap();
    assert!(verify_can_access_file(rustdb_common::byte_format!("{link}/file")).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}
