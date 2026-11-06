//! Native C++ COPY projection construction and rendering for byte names.
#[path = "../examples/native_copy_plan.rs"]
#[allow(dead_code, reason = "reuse the oracle CLI comparison routine")]
mod codec;
#[test]
fn native_json_copy_matches_cpp() {
    for line in include_str!("data/native_copy_plan_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
