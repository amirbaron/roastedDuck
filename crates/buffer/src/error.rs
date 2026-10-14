//! Error construction shared by the buffer manager.

use rustdb_common::{Error, ErrorKind};

const TUNING_LINK: &str = "https://duckdb.org/docs/stable/guides/performance/how_to_tune_workloads";

/// An `Out of Memory Error` with DuckDB's standard advice appended
/// (`OutOfMemoryException::ExtendOutOfMemoryError`).
pub fn out_of_memory(message: impl Into<String>) -> Error {
    let mut message = message.into();
    if !message.contains(TUNING_LINK) {
        message.push_str(
            "\n\nPossible solutions:\n\
             * Reducing the number of threads (SET threads=X)\n\
             * Disabling insertion-order preservation (SET preserve_insertion_order=false)\n\
             * Increasing the memory limit (SET memory_limit='...GB')\n\
             \nSee also ",
        );
        message.push_str(TUNING_LINK);
    }
    Error::new(ErrorKind::OutOfMemory, message)
}

/// An `IO Error`.
pub(crate) fn io_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_memory_text_matches_duckdb() {
        let error = out_of_memory("could not allocate block of size 256.0 KiB (1.8 MiB/1.9 MiB used)");
        assert_eq!(
            error.to_string(),
            "Out of Memory Error: could not allocate block of size 256.0 KiB (1.8 MiB/1.9 MiB used)\n\n\
             Possible solutions:\n\
             * Reducing the number of threads (SET threads=X)\n\
             * Disabling insertion-order preservation (SET preserve_insertion_order=false)\n\
             * Increasing the memory limit (SET memory_limit='...GB')\n\n\
             See also https://duckdb.org/docs/stable/guides/performance/how_to_tune_workloads"
        );
        // Extending twice is a no-op.
        let again = out_of_memory(error.raw_message().to_owned());
        assert_eq!(again, error);
    }
}
