"""Run the unchanged frozen regression body despite its parquet version-attribute guard.

The unmodified pytest module skips on both engines because pyarrow.parquet has no
__version__. This separate functional journey is not an unmodified pytest pass.
"""
import argparse
import ast
import hashlib
import json
from pathlib import Path
import tempfile

import duckdb
import pyarrow as pa
import pyarrow.parquet as pq

parser = argparse.ArgumentParser()
parser.add_argument('source', type=Path)
args = parser.parse_args()
source = args.source.read_bytes()
tree = ast.parse(source)
classes = [node for node in tree.body if isinstance(node, ast.ClassDef)]
assert len(classes) == 1 and classes[0].name == 'Test7652'
namespace = dict(pa=pa, pq=pq, tempfile=tempfile)
exec(compile(ast.Module(body=classes, type_ignores=[]), str(args.source), 'exec'), namespace)
with duckdb.connect() as connection:
    namespace['Test7652']().test_7652(connection)
print(json.dumps({'body_passed': True, 'source_sha256': hashlib.sha256(source).hexdigest(),
                  'pyarrow_version': pa.__version__, 'parquet_version_attribute': getattr(pq, '__version__', None),
                  'scope': 'Unchanged class/method AST with real pyarrow/parquet modules; module-level import guard omitted; not an unmodified pytest pass'}))
