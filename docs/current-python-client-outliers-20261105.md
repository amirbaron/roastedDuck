# Current Python client measurement outliers

Source2551; normal signed Mac arm64 wheels2593; independent ABI reviews2598. AllfiveMacPython cells are qualified. These client microbenchmarks are separate from TPC-H/ClickBench and whole-file SQL measurements; final performance acceptance remains pending.

Each row scans one million nullable pandas strings `["duck", "雪", None, "quack"]` at one engine thread and computes `count(x), sum(length(x))`. Timing includes `from_df`, aggregate execution and `fetchone`; dataframe construction, an eight-row warmup and garbage collection precede timing. Exact answers are checked. Three fresh processes per engine/profile are retained. Whole-process RSS includes the interpreter, dataframe and dependencies.

| Python / Arrow | C++ median ms | Rust median ms | Rust/C++ time | C++ max RSS MB | Rust max RSS MB | Rust/C++ max RSS |
|---|---:|---:|---:|---:|---:|---:|
|cp310 / 19|35.77|42.38|1.185|147.55|147.05|0.997|
|cp310 / 21|35.25|42.93|1.218|144.95|144.13|0.994|
|cp311 / 19|34.76|41.82|1.203|152.78|150.31|0.984|
|cp311 / 21|35.75|42.89|1.200|150.26|149.42|0.994|
|cp312 / 19|34.77|43.95|1.264|152.50|151.11|0.991|
|cp312 / 21|36.64|44.31|1.209|151.19|149.09|0.986|
|cp313 / 19|34.55|43.05|1.246|142.43|141.13|0.991|
|cp313 / 21|33.98|42.10|1.239|139.92|139.00|0.993|
|cp314 / 22|34.67|43.59|1.257|170.43|169.87|0.997|

The Python-backed scans flagged here are 18.5–26.4% slower, 6.6–9.2ms longer, with similar whole-process peak RSS. Their incremental-RSS differences remain in raw receipts and are not represented as the accepted60–70% macro memory saving. Arrow-backed gains stay in separate workload rows; no average combines them to hide slower Python-backed paths.

All18client workloads and108samples remain in the complete [five-ABI index](../.context/artifacts/platform-matrix-20261029/final-Mac-Python-five-ABI-index-2649/verification.json).

Evidence: [cp310 complete measurements and native identities](../.context/artifacts/platform-matrix-20261029/macos-cp310-functional-review-2598/verification.json).
Evidence: [cp311 complete measurements and native identities](../.context/artifacts/platform-matrix-20261029/macos-cp311-functional-review-2598/verification.json).
Evidence: [cp312 complete measurements and native identities](../.context/artifacts/platform-matrix-20261029/macos-cp312-functional-review-2598/verification.json).
Evidence: [cp313 complete measurements and native identities](../.context/artifacts/platform-matrix-20261029/macos-cp313-functional-review-2598/verification.json).
Evidence: [cp314 complete measurements and native identities](../.context/artifacts/platform-matrix-20261029/macos-cp314-functional-review-2598/verification.json).

Linux arm64 CPython3.10 is the first independently qualified current-source Linux cell. Its four client workloads are shown individually below. The Python-backed paths are 10.1% and 15.5% slower with approximately 11% less whole-process RSS; this is not the accepted native macro memory saving. Arrow-backed gains remain separate. CPython3.11 has also independently qualified; its Python-backed paths are 12.2% and 11.2% slower with approximately 10.4% less whole-process RSS. CPython3.12 has also independently qualified; its Python-backed paths are 12.0% and 6.8% slower with approximately 10.5% and 10.4% less whole-process RSS. CPython3.13 has also independently qualified; its Python-backed paths are 16.9% and 16.4% slower with approximately 11.1% and 10.8% less whole-process RSS. CPython3.14 has also independently qualified; its Python-backed Arrow22 workload is 11.5% slower with approximately 13.3% less whole-process RSS. All ten current Linux normal ABI cells are independently qualified; no normal ABI cell remains.

| Python | Arrow | Backing storage | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---|---|---:|---:|
|cp310|19|python|1.101|0.890|
|cp310|19|pyarrow|0.159|0.692|
|cp310|21|python|1.155|0.889|
|cp310|21|pyarrow|0.152|0.702|
|cp311|19|python|1.122|0.896|
|cp311|19|pyarrow|0.144|0.698|
|cp311|21|python|1.112|0.896|
|cp311|21|pyarrow|0.141|0.710|
|cp312|19|python|1.120|0.895|
|cp312|19|pyarrow|0.153|0.742|
|cp312|21|python|1.068|0.896|
|cp312|21|pyarrow|0.146|0.751|
|cp313|19|python|1.169|0.889|
|cp313|19|pyarrow|0.147|0.743|
|cp313|21|python|1.164|0.892|
|cp313|21|pyarrow|0.161|0.753|
|cp314|22|python|1.115|0.867|
|cp314|22|pyarrow|0.190|0.763|

Evidence: [current Linux arm64 CPython3.10 review](../.context/artifacts/platform-matrix-20261029/linux-arm64-cp310-functional-review-2697/verification.json). Raw samples and exact native identities are retained; these client measurements do not establish final TPC-H/ClickBench acceptance.

Evidence: [current Linux arm64 CPython3.11 review](../.context/artifacts/platform-matrix-20261029/linux-arm64-cp311-functional-review-2697/verification.json).

Evidence: [current Linux arm64 CPython3.12 review](../.context/artifacts/platform-matrix-20261029/linux-arm64-cp312-functional-review-2697/verification.json).

Evidence: [current Linux arm64 CPython3.13 review](../.context/artifacts/platform-matrix-20261029/linux-arm64-cp313-functional-review-2697/verification.json).

Evidence: [current Linux arm64 CPython3.14 review](../.context/artifacts/platform-matrix-20261029/linux-arm64-cp314-functional-review-2697/verification.json). The [complete arm64 five-cell index](../.context/artifacts/platform-matrix-20261029/current-Linux-ARM-Python-five-cell-index-2836/verification.json) recomputes all18 individual workload ratios from108 raw samples and checks each selected Rust native SHA/sole loaded image. Original sample provenance-label text is retained; actual native identities and signed review pins establish source scope.

Current Linux x86_64 CPython3.10 is independently qualified. Execution uses Rosetta, so these results establish translated-client observations rather than native x86_64 or TPC-H/ClickBench acceptance. Python-backed Arrow19 and Arrow21 medians are 0.8% and 6.7% slower with approximately 8.5% and 8.3% less maximum whole-process RSS. Best and median time ratios are both retained; Arrow-backed gains remain separate.

| Arrow | Backing storage | Rust/C++ best time | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---|---:|---:|---:|
|19|python|1.078|1.008|0.915|
|19|pyarrow|0.170|0.185|0.754|
|21|python|1.117|1.067|0.917|
|21|pyarrow|0.158|0.157|0.756|

Evidence: [current x86_64 CPython3.10 review](../.context/artifacts/platform-matrix-20261029/linux-amd64-cp310-functional-review-2697/verification.json), [independent XML/sample/identity recomputation](../.context/artifacts/platform-matrix-20261029/first-current-Linux-AMD-Python-cell-qualified-2882/verification.json). All24 raw samples and native image identities are retained.

Current Linux x86_64 CPython3.11 is independently qualified under Rosetta. Python-backed Arrow19 and Arrow21 medians are 2.8% and 1.0% slower with approximately 7.5% and 7.8% less maximum whole-process RSS. These are individual translated-client observations; they do not establish native x86_64 or TPC-H/ClickBench acceptance.

| Arrow | Backing storage | Rust/C++ best time | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---|---:|---:|---:|
|19|python|1.032|1.028|0.925|
|19|pyarrow|0.163|0.164|0.760|
|21|python|1.052|1.010|0.922|
|21|pyarrow|0.157|0.160|0.761|

Evidence: [current x86_64 CPython3.11 review](../.context/artifacts/platform-matrix-20261029/linux-amd64-cp311-functional-review-2697/verification.json), [independent XML/sample/identity recomputation](../.context/artifacts/platform-matrix-20261029/second-current-Linux-AMD-Python-cell-qualified-2884/verification.json). All24 raw samples remain separate; no averaging with Arrow-backed gains.

Current Linux x86_64 CPython3.12 is independently qualified under Rosetta. Python-backed Arrow19 and Arrow21 medians are 12.9% and 16.9% slower with approximately 7.9% and 7.6% less maximum whole-process RSS. Best-time ratios are retained beside medians; Arrow-backed gains remain separate. These are translated-client observations, not native x86_64 or TPC-H/ClickBench acceptance.

| Arrow | Backing storage | Rust/C++ best time | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---|---:|---:|---:|
|19|python|1.127|1.129|0.921|
|19|pyarrow|0.168|0.174|0.790|
|21|python|1.201|1.169|0.924|
|21|pyarrow|0.172|0.169|0.796|

Evidence: [current x86_64 CPython3.12 review](../.context/artifacts/platform-matrix-20261029/linux-amd64-cp312-functional-review-2697/verification.json), [independent XML/sample/identity recomputation](../.context/artifacts/platform-matrix-20261029/third-current-Linux-AMD-Python-cell-qualified-2886/verification.json). All24 raw samples remain separate; no averaging across backends or ABIs hides slower paths.

Current Linux x86_64 CPython3.13 is independently qualified under Rosetta. Python-backed Arrow19 and Arrow21 medians are 4.4% and 8.9% slower with approximately 8.2% and 7.9% less maximum whole-process RSS. Individual best-time ratios and Arrow-backed gains remain separate. These observations do not establish native x86_64 or TPC-H/ClickBench acceptance.

| Arrow | Backing storage | Rust/C++ best time | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---|---:|---:|---:|
|19|python|1.076|1.044|0.918|
|19|pyarrow|0.170|0.169|0.790|
|21|python|1.124|1.089|0.921|
|21|pyarrow|0.166|0.167|0.798|

Evidence: [current x86_64 CPython3.13 review](../.context/artifacts/platform-matrix-20261029/linux-amd64-cp313-functional-review-2697/verification.json), [independent XML/sample/identity recomputation](../.context/artifacts/platform-matrix-20261029/fourth-current-Linux-AMD-Python-cell-qualified-2888/verification.json). All24 raw samples are preserved individually.

Current Linux x86_64 CPython3.14 is independently qualified under Rosetta. Its Python-backed Arrow22 median time ratio is0.889 with maximum whole-process RSS ratio0.896; the Arrow-backed result stays separate. These are translated-client observations, not native x86_64 or TPC-H/ClickBench acceptance.

| Arrow | Backing storage | Rust/C++ best time | Rust/C++ median time | Rust/C++ max whole RSS |
|---|---|---:|---:|---:|
|22|python|0.894|0.889|0.896|
|22|pyarrow|0.185|0.178|0.802|

Evidence: [current x86_64 CPython3.14 review](../.context/artifacts/platform-matrix-20261029/linux-amd64-cp314-functional-review-2697/verification.json), [independent final-cell sample check](../.context/artifacts/platform-matrix-20261029/final-current-Linux-AMD-Python-cell-qualified-2891/verification.json), [all36 Linux workload rows and216 recomputed raw samples](../.context/artifacts/platform-matrix-20261029/current-Linux-Python-all-ten-index-2892/verification.json). Best/median/max whole-RSS results remain individual across all architectures, ABIs and backends.
