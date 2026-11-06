"""Two real renderers with a Python UDF callback, bounded by a parent timeout."""
import json
import threading
import time

import duckdb

entered, release = threading.Event(), threading.Event()
calls, outputs, errors = [], [], []
con = duckdb.connect(config={'threads': 1})

def slow(value):
    calls.append(value)
    entered.set()
    if not release.wait(5):
        raise RuntimeError('parent did not release callback')
    return value

con.create_function('render_callback', slow, ['BIGINT'], 'BIGINT', side_effects=True)
relation = con.sql('SELECT render_callback(i) AS i FROM range(1) t(i)')

def render():
    try:
        outputs.append(repr(relation))
    except BaseException as error:
        errors.append((type(error).__name__, str(error)))

first = threading.Thread(target=render)
first.start()
assert entered.wait(5)
second = threading.Thread(target=render)
second.start()
time.sleep(0.05)
release.set()
first.join(5)
second.join(5)
assert not first.is_alive() and not second.is_alive()
assert not errors, errors
assert len(outputs) == 2 and outputs[0] == outputs[1]
assert calls and all(value == 0 for value in calls), calls
con.close()
try:
    cached_after_close = repr(relation) == outputs[0]
except Exception as error:
    cached_after_close = {'error': type(error).__name__, 'message': str(error)}
print(json.dumps({'renderers': len(outputs), 'callbacks': calls,
                  'same_output': True, 'cached_after_close': cached_after_close}))
