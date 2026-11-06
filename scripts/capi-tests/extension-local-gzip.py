"""Compare local gzip INSTALL with C++: first member, CRC, headers and failure cleanup.

The caller authenticates the library and raw-v1/raw-v2 C-only fixtures before
and after execution. Generated inputs contain synthetic or authenticated bytes.
"""
import argparse
import ctypes
import datetime
import hashlib
import json
from pathlib import Path

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('fixtures', type=Path)
    parser.add_argument('platform')
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    library = args.library.resolve()
    fixture = args.fixtures.resolve()
    platform = args.platform
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)

    def sha(p):
        with p.open('rb') as f:
            return hashlib.file_digest(f, 'sha256').hexdigest()
    lib = ctypes.CDLL(str(library))
    vp = ctypes.c_void_p
    pp = ctypes.POINTER(vp)

    class Result(ctypes.Structure):
        _fields_ = [('cols', ctypes.c_uint64), ('rows', ctypes.c_uint64), ('changed', ctypes.c_uint64), ('columns', vp), ('error', vp), ('internal', vp)]
    lib.duckdb_library_version.restype = ctypes.c_char_p
    assert lib.duckdb_library_version() == b'v1.5.6'
    for name, args, restype in [('duckdb_create_config', [pp], ctypes.c_int), ('duckdb_set_config', [vp, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_int), ('duckdb_open_ext', [ctypes.c_char_p, pp, vp, pp], ctypes.c_int), ('duckdb_connect', [vp, pp], ctypes.c_int), ('duckdb_query', [vp, ctypes.c_char_p, ctypes.POINTER(Result)], ctypes.c_int), ('duckdb_column_count', [ctypes.POINTER(Result)], ctypes.c_uint64), ('duckdb_row_count', [ctypes.POINTER(Result)], ctypes.c_uint64), ('duckdb_result_error', [ctypes.POINTER(Result)], ctypes.c_char_p), ('duckdb_value_varchar', [ctypes.POINTER(Result), ctypes.c_uint64, ctypes.c_uint64], vp)]:
        f = getattr(lib, name)
        f.argtypes = args
        f.restype = restype
    lib.duckdb_free.argtypes = [vp]
    lib.duckdb_destroy_result.argtypes = [ctypes.POINTER(Result)]
    lib.duckdb_destroy_config.argtypes = [pp]
    lib.duckdb_disconnect.argtypes = [pp]
    lib.duckdb_close.argtypes = [pp]
    records = []

    def query(label, sql, expect_success=True):
        r = Result()
        code = lib.duckdb_query(con, sql.encode(), ctypes.byref(r))
        error = lib.duckdb_result_error(ctypes.byref(r))
        data = []
        if code == 0:
            for row in range(lib.duckdb_row_count(ctypes.byref(r))):
                values = []
                for col in range(lib.duckdb_column_count(ctypes.byref(r))):
                    value = lib.duckdb_value_varchar(ctypes.byref(r), col, row)
                    values.append(ctypes.string_at(value).decode() if value else None)
                    if value:
                        lib.duckdb_free(value)
                data.append(values)
        record = {'label': label, 'sql': sql, 'status': code, 'error': error.decode() if error else None, 'rows': data}
        records.append(record)
        lib.duckdb_destroy_result(ctypes.byref(r))
        (out / 'progress.json').write_text(json.dumps(records, indent=2) + '\n')
        if expect_success:
            assert code == 0, record
        return record
    quote = lambda path: str(path).replace("'", "''")
    import gzip, io
    raw = (fixture / 'raw-v1/c_only_probe.duckdb_extension').read_bytes()
    second = (fixture / 'raw-v2/c_only_probe.duckdb_extension').read_bytes()
    compressed = gzip.compress(raw, mtime=0)
    cases = []
    for kind in ['gzip', 'concat', 'crc', 'filename', 'extra', 'ascii', 'method', 'short-header', 'truncated', 'empty']:
        case = out / kind
        case.mkdir()
        source = case / 'c_only_probe.duckdb_extension.gz'
        data = compressed
        if kind == 'concat':
            data += gzip.compress(second, mtime=0)
        if kind == 'crc':
            data = data[:-8] + bytes([data[-8] ^ 1]) + data[-7:]
        if kind == 'filename':
            data = data[:3] + bytes([data[3] | 8]) + data[4:10] + b'original-name\x00' + data[10:]
        if kind == 'extra':
            data = data[:3] + bytes([data[3] | 4]) + data[4:10] + bytes(2) + data[10:]
        if kind == 'ascii':
            data = data[:3] + bytes([data[3] | 1]) + data[4:]
        if kind == 'method':
            data = data[:2] + b'\t' + data[3:]
        if kind == 'short-header':
            data = data[:8]
        if kind == 'truncated':
            data = data[:len(data) // 2]
        if kind == 'empty':
            data = gzip.compress(b'', mtime=0)
        source.write_bytes(data)
        directory = case / 'extensions'
        start = len(records)
        config = vp()
        db = vp()
        con = vp()
        err = vp()
        assert lib.duckdb_create_config(ctypes.byref(config)) == 0
        for key, value in [(b'allow_unsigned_extensions', b'true'), (b'extension_directory', str(directory).encode())]:
            assert lib.duckdb_set_config(config, key, value) == 0
        assert lib.duckdb_open_ext(None, ctypes.byref(db), config, ctypes.byref(err)) == 0
        lib.duckdb_destroy_config(ctypes.byref(config))
        assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
        result = query(kind + '-install', "INSTALL '" + quote(source) + "'", False)
        path = directory / 'v1.5.6' / platform / 'c_only_probe.duckdb_extension'
        if result['status'] == 0:
            assert path.read_bytes() == raw
            query(kind + '-load', 'LOAD c_only_probe')
            assert query(kind + '-function', 'SELECT c_only_add7(5)')['rows'] == [['12']]
        else:
            assert not path.exists() and (not Path(str(path) + '.info').exists())
        assert not list(directory.rglob('*.tmp-*'))
        assert query(kind + '-database-valid', 'SELECT 42')['rows'] == [['42']]
        lib.duckdb_disconnect(ctypes.byref(con))
        lib.duckdb_close(ctypes.byref(db))
        cases.append({'kind': kind, 'records': records[start:]})
    (out / 'verification.json').write_text(json.dumps({'cases': cases, 'scope': 'Public C API localINSTALLgzipfirstmember/header/CRCcontract; inputsderivedfromauthenticatednativefixtures; allpublishedsuccessbodiesmustexactlyequaltheauthenticatedv1beforeLOAD.', 'utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}, indent=2) + '\n')
    print([(c['kind'], c['records'][0]['status']) for c in cases], flush=True)
if __name__ == '__main__':
    main()
