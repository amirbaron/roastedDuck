"""Private worker for encrypted_compat.py; native images are authenticated by its caller.

The creator deliberately uses os._exit after committed writes to retain a real WAL.
Expected typed values are independent Python literals, not copied from either engine.
"""
from pathlib import Path
import ctypes, datetime, hashlib, importlib.util, json, os, resource, sys
w = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('control', w / 'scripts/httpfs-tests/s3_version_control.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
case, library, package, stage, mode = (Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4], sys.argv[5])
cipher, version = sys.argv[6:8] if len(sys.argv) == 8 else ('GCM', 'v1')
assert cipher in ('GCM', 'CTR') and version in ('v0', 'v1')
resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
api = m.API(library, case / (stage + '-secrets'))
records = []

def query(sql, ok=True):
    r = api.query(sql)
    records.append(r)
    assert (r['status'] == 0) == ok, r
    return r['rows']
q = m.quote
key = 'duckdb-rust-encrypted-compat-test-key'
file = case / 'encrypted.duckdb'
txt1 = 'producer-' + '雪x' * 10000
txt2 = 'producer-two'
txt3 = 'receiver-marker'

def epoch(value):
    delta = datetime.datetime.fromisoformat(value) - datetime.datetime(1970, 1, 1)
    return str(delta.days * 86400000000 + delta.seconds * 1000000 + delta.microseconds)

def digest(value):
    return hashlib.md5(value.encode()).hexdigest()

def expected(modified=False):
    rows = [['1', str(len(txt1)), digest(txt1), '12345678901234567890.123456789', '00112233-4455-6677-8899-aabbccddeeff', '00FF10', epoch('2024-01-02T03:04:05.123456'), '[1, NULL, 3]'], ['2', str(len('receiver-two' if modified else txt2)), digest('receiver-two' if modified else txt2), None, None, '', None, '[]']]
    if modified:
        rows.append(['3', str(len(txt3)), digest(txt3), '-7.500000000', 'fedcba98-7654-3210-ffff-000000000001', '012345', epoch('1988-06-07T08:09:10.000011'), '[-1, 0]'])
    return rows
select = 'SELECT id,length(txt),md5(txt),d::VARCHAR,u::VARCHAR,hex(payload),epoch_us(ts),a::VARCHAR FROM d.t ORDER BY id'
query(f"SET extension_directory='{q(case / (stage + '-extensions'))}'")
query("SET TimeZone='UTC'")
query(f"LOAD '{q(package)}'")
query("SET wal_autocheckpoint='1TB'")
if stage != 'create':
    wrong = api.query(f"ATTACH '{q(file)}' AS wrong (ENCRYPTION_KEY 'incorrect-key', ENCRYPTION_CIPHER '{cipher}', debug_encryption_version '{version}')")
    records.append(wrong)
    assert wrong['status'] != 0 and 'Wrong encryption key used' in wrong['error'], wrong
query(f"ATTACH '{q(file)}' AS d (ENCRYPTION_KEY '{key}', ENCRYPTION_CIPHER '{cipher}', debug_encryption_version '{version}')")
if stage == 'create':
    query('CREATE TABLE d.t(id INTEGER PRIMARY KEY,txt VARCHAR,d DECIMAL(38,9),u UUID,payload BLOB,ts TIMESTAMP,a INTEGER[])')
    query("INSERT INTO d.t VALUES(1,'base',12345678901234567890.123456789,'00112233-4455-6677-8899-aabbccddeeff'::UUID,from_hex('00FF10'),TIMESTAMP '2024-01-02 03:04:05.123456',[1,NULL,3])")
    query('CREATE INDEX text_idx ON d.t(txt)')
    query('CHECKPOINT d')
    query(f"INSERT INTO d.t VALUES(2,'{txt2}',NULL,NULL,from_hex(''),NULL,[])")
    query(f"UPDATE d.t SET txt='{q(txt1)}' WHERE id=1")
    if mode == 'checkpoint':
        query('CHECKPOINT d')
    assert query(select) == expected(), records[-1]
elif stage == 'receive':
    assert query(select) == expected(), records[-1]
    query(f"INSERT INTO d.t VALUES(3,'{txt3}',-7.500000000,'fedcba98-7654-3210-ffff-000000000001'::UUID,from_hex('012345'),TIMESTAMP '1988-06-07 08:09:10.000011',[-1,0])")
    query("UPDATE d.t SET txt='receiver-two' WHERE id=2")
    query('CHECKPOINT d')
    assert query(select) == expected(True), records[-1]
elif stage == 'recover':
    rows = query(select)
    target = expected()
    if mode == 'truncate_one_byte':
        target[0][1] = str(len('base'))
        target[0][2] = digest('base')
    else:
        assert mode == 'append_one_byte', mode
    assert rows == target, records[-1]
else:
    assert stage == 'readback'
    assert query(select) == expected(True), records[-1]
    assert query("SELECT id FROM d.t WHERE txt='receiver-two'") == [['2']]
    assert query(f"SELECT id FROM d.t WHERE txt='{q(txt1)}'") == [['1']]
if sys.platform == 'darwin':
    system = ctypes.CDLL(None)
    system._dyld_image_count.restype = ctypes.c_uint32
    system._dyld_get_image_name.argtypes = [ctypes.c_uint32]
    system._dyld_get_image_name.restype = ctypes.c_char_p
    images = [system._dyld_get_image_name(i).decode() for i in range(system._dyld_image_count())]
else:
    import re
    images = []
    for line in Path('/proc/self/maps').read_text().splitlines():
        parts = line.split(None, 5)
        if len(parts) == 6:
            images.append(re.sub('\\\\([0-7]{3})', lambda m: chr(int(m[1], 8)), parts[5]))
engines = sorted({str(Path(x).resolve()) for x in images if Path(x).name.startswith(('libduckdb.', '_duckdb'))})
assert engines == [str(library.resolve())], engines
(case / (stage + '-result.json')).write_text(json.dumps({'stage': stage, 'mode': mode, 'cipher': cipher, 'version': version, 'library': str(library), 'engine_images': engines, 'queries': records, 'expected_checked': True, 'hard_exit_to_preserve_wal': stage == 'create'}, indent=2) + '\n')
if stage == 'create':
    os._exit(0)
api.close()
