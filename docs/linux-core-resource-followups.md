# Linux core resource follow-ups

The first complete paired Linux arm64 corpus run used a two-CPU container with
7 GiB RAM and swap disabled. Both engines were killed by that container's memory
limit on the large-gzip regression; the C++ VARIANT TPC-H control also exceeded the
300-second limit. Those original results remain preserved. They are not silently
changed to passes.

## Large gzip round trip

Both current signed engines pass the unchanged
`test/sql/copy/csv/test_copy_gzip_large_block.test_slow` with 7 GiB RAM and an
explicit8 GiB SSD-swap allowance. The test writes2048 rows of2,100,000 characters,
then reads the gzip stream back and checks the exact count and4,300,800,000-character
sum. Its input hash remains unchanged before and after each engine.

| Engine | Correctness | Container memory peak | Container swap peak | Diagnostic wall time |
|---|---|---|---|---|
| C++ |Pass|7 GiB cap|1,391,775,744 bytes|80.22 s|
| Rust |Pass|7 GiB cap|2,471,325,696 bytes|61.77 s|

Both fresh container cgroups report zero OOM/OOM-kill events. These are cgroup
memory/swap peaks, not process peak RSS. The swap peaks are reported separately;
their difference remains an explicit resource outlier. Timings with active swap
are correctness diagnostics, not TPC-H/ClickBench or speed–memory acceptance.
The engine's cache policy and default no-swap benchmark conditions are unchanged.

The Rust images are the current optimized AVG artifacts under
`work/linux-arm64/average-release-signed-1/`; the reference is
`work/linux-arm64/reference-release-signed-2/`. Native engines, the C++ oracle and
all signed C++ runtime assets verify before and after. The C++ oracle's loader
resolves the exact selected reference library.

## VARIANT TPC-H

Both engines pass the unchanged VARIANT SF1 storage/checkpoint/restart test and
all22 official query answers with the original7 GiB RAM/no-swap limits and a
900-second correctness timeout. The SQL plus22 answer files have recorded hashes
checked before and after each engine. Both report zero OOM events and zero swap.

| Engine | Correctness | Container memory peak | Diagnostic wall time |
|---|---|---|---|
| C++ |Pass|3,732,500,480 bytes|332.21 s|
| Rust |Pass|3,610,292,224 bytes|110.73 s|

The C++ control exceeds the original300-second limit, explaining its prior timeout.
These whole-file diagnostic times include data generation, table construction,
checkpoint/restart and the query set; they are not individual-query benchmarks or
final macro performance acceptance. No query, expected answer or engine memory
policy was changed to obtain a pass.

## Evidence

All artifacts are on the dedicated SSD beneath
`.context/artifacts/platform-matrix-20261029/`:

- `work/core-followup-inputs-2.json`: SQL/answer-file hashes.
- `linux-arm64-large-gzip-followup-2/terminal.json`: both actual successful exits.
- `work/linux-arm64/large-gzip-followup-{cpp,rust}-2/verification.json`: exact
  results, commands, cgroup limits/events, swap peaks and signatures.
- `linux-arm64-variant-tpch-followup-2/terminal.json`: both actual successful exits.
- `work/linux-arm64/variant-tpch-followup-{cpp,rust}-2/verification.json`: exact
  results, cgroup limits/events, hashes and signatures for the longer-timeout run.

Final full-corpus, shell, selected-client, storage/WAL, platform and coupled macro
performance acceptance remain open. These diagnostic profiles do not rewrite the
preserved canonical run or its resource limits.
