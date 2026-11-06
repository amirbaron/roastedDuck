"""Generate Rust startup registration definitions from frozen DuckDB 1.5.6 sources."""

import argparse
import json
from pathlib import Path
import re
import subprocess


def definitions(root, source, include):
    text = (root / source).read_text()
    names = {}
    for header in re.findall(r'#include "([^"]+functions.hpp)"', text):
        path = root / include / header
        if not path.exists():
            path = root / "src/include" / header
        for name, body in re.findall(r"struct (\w+)\s*\{(.*?)\n\};", path.read_text(), re.S):
            value = re.search(r'const char \*Name = ("(?:\\.|[^"\\])*")', body)
            if value:
                names[name] = json.loads(value[1])
    entries = []
    for macro, name in re.findall(
        r"^\s*(DUCKDB_(?:SCALAR|AGGREGATE)_FUNCTION(?:_SET)?(?:_ALIAS)?)\((\w+)\),",
        text, re.M,
    ):
        kind = "Aggregate" if "AGGREGATE" in macro else "Scalar"
        entries.append((kind, names[name]))
    if not entries or len(entries) != len(set(entries)):
        raise ValueError(f"Empty or duplicate registration definitions in {source}")
    return entries


def main():
    workspace = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path,
                        default=workspace / ".context/duckdb-download/duckdb-1.5.6")
    parser.add_argument("--output", type=Path,
                        default=workspace / "crates/rustdb/src/function_lists.rs")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    text = (
        "//! Registration definitions from DuckDB 1.5.6's generated `src/function/function_list.cpp`\n"
        "//! and `extension/core_functions/function_list.cpp`, in source order. No catalog IDs.\n"
        "//! Regenerate with `scripts/generate-function-lists.py`.\n\n"
        "#[derive(Clone, Copy)]\n"
        "pub(super) enum FunctionKind {\n    Scalar,\n    Aggregate,\n}\n\n"
    )
    for source, include, constant in [
        ("src/function/function_list.cpp", "src/include", "BUILTIN_FUNCTIONS"),
        ("extension/core_functions/function_list.cpp", "extension/core_functions/include", "CORE_FUNCTIONS"),
    ]:
        entries = definitions(args.source, source, include)
        text += f"pub(super) const {constant}: &[(FunctionKind, &str)] = &[\n"
        for kind, name in entries:
            text += f"    (FunctionKind::{kind}, {json.dumps(name)}),\n"
        text += "];\n\n"
    overloads = (args.source / "src/include/duckdb/main/extension_entries.hpp").read_text()
    overloads = overloads.split("EXTENSION_FUNCTION_OVERLOADS[] = {", 1)[1].split("};", 1)[0]
    records = re.findall(
        r'\{\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*CatalogType::SCALAR_FUNCTION_ENTRY\s*,\s*"([^"]+)"\s*\}',
        overloads,
    )
    if len(records) != overloads.count("CatalogType::") or not records:
        raise ValueError("Unparsed extension function overload definitions")
    text += "/// Typed auto-loading entries, grouped in frozen source order.\n"
    text += "pub(super) const EXTENSION_OVERLOADS: &[(&str, &str, &str)] = &[\n"
    for name, extension, signature in records:
        text += f"    ({json.dumps(name)}, {json.dumps(extension)}, {json.dumps(signature)}),\n"
    text += "];\n"
    formatted = subprocess.run(
        ["rustfmt", "--edition", "2024", "--emit", "stdout"],
        input=text, capture_output=True, text=True, check=True,
    ).stdout
    if args.check:
        if args.output.read_text() != formatted:
            raise SystemExit("Startup function lists differ from the source definitions")
    else:
        args.output.write_text(formatted)


if __name__ == "__main__":
    main()
