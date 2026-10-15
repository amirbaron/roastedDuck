//! The test environment (port of the default `TestConfiguration` and the path helpers in
//! `test/helpers/test_helpers.cpp`).
//!
//! The harness always runs DuckDB's default configuration: no `--test-config` file, no
//! `DUCKDB_TEST_*` overrides. What remains is the per-process temporary directory, the test
//! variables (`{TEST_DIR}`, `${DATA_DIR}`, ...) and the `--require` list.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

/// Name of the per-run temporary directory, relative to the working directory.
pub const TESTING_DIRECTORY_NAME: &str = "duckdb_unittest_tempdir";

/// Process-wide settings shared by every file a worker runs.
#[derive(Clone, Debug)]
pub struct TestEnvironment {
    working_dir: String,
    test_dir: String,
    build_dir: String,
    test_uuid: String,
    required: BTreeSet<String>,
}

impl TestEnvironment {
    /// Captures the current working directory (which must be the DuckDB source root) and uses
    /// `duckdb_unittest_tempdir/<pid>` as the test directory.
    pub fn new(build_dir: &str, required: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let working_dir = std::env::current_dir()?.to_string_lossy().into_owned();
        Ok(Self {
            working_dir,
            test_dir: format!("{TESTING_DIRECTORY_NAME}/{}", std::process::id()),
            build_dir: build_dir.to_owned(),
            test_uuid: uuid::Uuid::new_v4().to_string(),
            required: required.into_iter().map(|r| r.to_ascii_lowercase()).collect(),
        })
    }

    /// `TestDirectoryPath()`: the per-process test directory, created on demand.
    pub fn test_directory(&self) -> &str {
        // Errors surface later, when the test touches the directory.
        let _ = fs::create_dir_all(&self.test_dir);
        &self.test_dir
    }

    /// `TestCreatePath(suffix)`.
    pub fn test_path(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.test_directory())
    }

    /// The absolute working directory (`FileSystem::GetWorkingDirectory()`).
    pub fn working_directory(&self) -> &str {
        &self.working_dir
    }

    pub fn build_directory(&self) -> &str {
        &self.build_dir
    }

    /// Empties and recreates the test directory (done before and after every file).
    pub fn reset_test_directory(&self) -> io::Result<()> {
        remove_dir_if_exists(Path::new(&self.test_dir))?;
        fs::create_dir_all(&self.test_dir)
    }

    /// Removes the test directory.
    pub fn remove_test_directory(&self) -> io::Result<()> {
        remove_dir_if_exists(Path::new(&self.test_dir))
    }

    /// `IsRequired`: whether `--require <name>` was given (case-insensitive).
    pub fn is_required(&self, name: &str) -> bool {
        self.required.contains(&name.to_ascii_lowercase())
    }

    /// `TestConfiguration::GetTestEnvMap()`: the standard test variables.
    pub fn test_variables(&self) -> BTreeMap<String, String> {
        let temp_dir = self.test_directory().to_owned();
        BTreeMap::from([
            ("TEST_UUID".to_owned(), self.test_uuid.clone()),
            ("BUILD_DIR".to_owned(), self.build_dir.clone()),
            ("WORKING_DIR".to_owned(), self.working_dir.clone()),
            ("DATA_DIR".to_owned(), format!("{}/data", self.working_dir)),
            ("CATALOG_DIR".to_owned(), format!("{temp_dir}/{}", self.test_uuid)),
            ("TEMP_DIR".to_owned(), temp_dir),
        ])
    }

    /// `TestConfiguration::ProcessPath`.
    pub fn process_path(&self, path: &str, test_name: &str) -> String {
        let test_dir = self.test_directory();
        path.replace("{TEST_DIR}", test_dir)
            .replace("{UUID}", &uuid::Uuid::new_v4().to_string())
            .replace("{TEST_NAME}", test_name)
            .replace("{BASE_TEST_NAME}", &test_name.replace('/', "_"))
            .replace("__TEST_DIR__", test_dir)
            .replace("__WORKING_DIRECTORY__", &self.working_dir)
    }

    /// `DeleteDatabase`: removes a database file and its WAL.
    pub fn delete_database(path: &str) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(format!("{path}.wal"));
    }
}

fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
