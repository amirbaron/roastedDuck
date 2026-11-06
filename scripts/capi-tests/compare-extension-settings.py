"""Compare setting journeys literally, with an explicit NULL-read native-frame exception."""

import argparse
import copy
import json
from pathlib import Path
import re


TRACE = "\n\nStack Trace:\n"
FOOTER = "\nThis error signals an assertion failure within DuckDB."
PARQUET_SETTINGS = {
    "binary_as_string", "disable_parquet_prefetching", "prefetch_all_parquet_files",
    "parquet_metadata_cache", "enable_geoparquet_conversion",
}


def trace_parts(error):
    if error.count(TRACE) != 1 or error.count(FOOTER) != 1:
        return None
    prefix, tail = error.split(TRACE, 1)
    trace, footer = tail.split(FOOTER, 1)
    indices = []
    for line in trace.splitlines():
        if not line.strip():
            continue
        if re.search(r"\b(?:Error|Exception):\s", line):
            return None
        match = re.match(r"^\s*(\d+)(?::\s+|\s{2,})(.+)$", line)
        if match:
            indices.append(int(match[1]))
        elif not indices or not line.lstrip().startswith("at "):
            return None
    if len(indices) < 3 or indices != list(range(len(indices))):
        return None
    return prefix, FOOTER + footer, trace, len(indices)


def native_frame_only(setting, left, right):
    if setting not in PARQUET_SETTINGS or left.get("stage") != "null" or right.get("stage") != "null":
        return False
    if set(left) != set(right) or set(left) != {"stage", "connections", "parquet_read"}:
        return False
    if left["connections"] != right["connections"]:
        return False
    reads_left, reads_right = left["parquet_read"], right["parquet_read"]
    if len(reads_left) != 2 or len(reads_right) != 2 or reads_left[0] != reads_right[0]:
        return False
    before, after = reads_left[1], reads_right[1]
    if {key: value for key, value in before.items() if key != "error"} != {
        key: value for key, value in after.items() if key != "error"
    }:
        return False
    if before.get("status") != 1 or before.get("error_type") != 31 or before.get("rows") != []:
        return False
    first, second = trace_parts(before.get("error") or ""), trace_parts(after.get("error") or "")
    if not first or not second or first[:2] != second[:2]:
        return False
    return ("GetValue" in first[2] and "Parquet" in first[2]
            and "setting_bool_checked" in second[2] and "crates/parquet/src/scan.rs" in second[2])


def negative_controls(setting, left, right):
    variants = []
    for field, value in [("status", 0), ("error_type", 32), ("rows", [["wrong"]])]:
        changed = copy.deepcopy(right)
        changed["parquet_read"][1][field] = value
        variants.append(changed)
    for transform in [
        lambda error: error.replace("Calling GetValueInternal", "Unexpected behavior", 1),
        lambda error: error.replace("This error signals", "Different footer", 1),
        lambda error: error.split(TRACE, 1)[0] + TRACE + FOOTER + error.split(FOOTER, 1)[1],
        lambda error: error.replace("setting_bool_checked", "unrelated_origin"),
        lambda error: error + "\nunexpected trailing error",
    ]:
        changed = copy.deepcopy(right)
        changed["parquet_read"][1]["error"] = transform(changed["parquet_read"][1]["error"])
        variants.append(changed)
    changed = copy.deepcopy(right)
    changed["connections"][1]["rows"][0][1] = ""
    variants.append(changed)
    changed = copy.deepcopy(right)
    changed["stage"] = "unrelated-case"
    variants.append(changed)
    assert all(not native_frame_only(setting, left, variant) for variant in variants)
    return len(variants)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cpp", type=Path)
    parser.add_argument("rust", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--allow-native-frames", action="store_true")
    args = parser.parse_args()
    if args.out.exists():
        parser.error("preserve evidence: output must be new")
    left, right = [json.loads(path.read_text())["records"] for path in [args.cpp, args.rust]]
    assert len(left) == len(right) == 5
    exact, native, rejected = 0, [], []
    for first, second in zip(left, right):
        assert first["setting"] == second["setting"]
        assert len(first["steps"]) == len(second["steps"])
        for before, after in zip(first["steps"], second["steps"]):
            assert before["stage"] == after["stage"]
            if before == after:
                exact += 1
            elif args.allow_native_frames and native_frame_only(first["setting"], before, after):
                controls = negative_controls(first["setting"], before, after)
                native.append({"setting": first["setting"], "stage": before["stage"],
                               "negative_controls_rejected": controls})
            else:
                rejected.append({"setting": first["setting"], "stage": before["stage"]})
    report = {"exact_raw_steps": exact, "native_frame_only_steps": native,
              "other_differences": rejected, "literal_identity": not native and not rejected,
              "native_frame_exception_explicit": args.allow_native_frames}
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"exact_raw_steps": exact, "native_frame_only_steps": len(native),
                      "other_differences": len(rejected), "literal_identity": report["literal_identity"]}))
    raise SystemExit(bool(rejected))


if __name__ == "__main__":
    main()
