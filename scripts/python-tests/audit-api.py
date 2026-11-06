"""Inventory actual frozen runtime exports without treating typing protocols as native APIs."""
import importlib
import json
from pathlib import Path
import sys

import duckdb


def resolve(name):
    # These stub namespaces declare types re-exported at the extension root;
    # they are not necessarily importable runtime submodules.
    for prefix in ["_duckdb._expression.", "_duckdb._enums."]:
        if name.startswith(prefix):
            name = "_duckdb." + name.removeprefix(prefix)
            break
    if name in ["_duckdb._expression", "_duckdb._enums"]:
        name = "_duckdb"
    parts = name.split(".")
    for count in range(len(parts), 0, -1):
        try:
            value = importlib.import_module(".".join(parts[:count]))
        except ModuleNotFoundError:
            continue
        for part in parts[count:]:
            value = getattr(value, part)
        return value
    raise ModuleNotFoundError(name)


inventory = json.loads(Path(sys.argv[1]).read_text())
results = {}
for item in inventory["surfaces"]:
    key = item["surface"] + "." + item["name"]
    try:
        value = getattr(resolve(item["surface"]), item["name"])
        results[key] = {"present": True, "callable": callable(value), "type": type(value).__name__}
    except (AttributeError, ModuleNotFoundError) as error:
        results[key] = {"present": False, "reason": type(error).__name__}
Path(sys.argv[2]).write_text(json.dumps({"exports": results}, indent=2) + "\n")
print(json.dumps({"items": len(results), "present": sum(value["present"] for value in results.values())}))
