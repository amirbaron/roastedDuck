//! Frozen C++ metadata wire and SQL for native function/window/collation/LOAD names.
#[path = "../examples/function_window_names.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_function_window_names_match_cpp() {
    for line in include_str!("data/function_window_names_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
