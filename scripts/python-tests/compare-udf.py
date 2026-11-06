"""Scalar UDF values, callback batching/ownership, transactions and error contracts."""
import gc
import json
from pathlib import Path
import sys
import weakref
import duckdb
import pyarrow as pa
import pyarrow.compute as pc

outcomes = {}

def capture(name, action):
    try:
        outcomes[name] = {'value': action()}
    except Exception as error:
        message = str(error)
        result = {'error': type(error).__name__, 'message': message.split('\n\nAt:')[0]}
        if 'Python exception occurred while executing the UDF:' in message:
            result['traceback_present'] = '\n\nAt:\n' in message
        outcomes[name] = result


def lifecycle(mode):
    con = duckdb.connect(config={'threads': 1})
    counts = []
    def f(x):
        counts.append(len(x) if mode == 'arrow' else 1)
        return pc.add(x, 7) if mode == 'arrow' else x + 7
    owner = weakref.ref(f)
    con.create_function('f', f, ['BIGINT'], 'BIGINT', type=mode, side_effects=True)
    del f
    gc.collect()
    retained = owner() is not None
    result = con.sql('select sum(f(case when i%3=0 then NULL else i end)) from range(5000)t(i)').fetchall()
    observed = sum(counts)
    con.remove_function('F')
    gc.collect()
    released = owner() is None
    con.close()
    return [result, observed, retained, released, max(counts) <= duckdb.__standard_vector_size__]


def nulls(mode, special):
    con = duckdb.connect(config={'threads': 1})
    def f(x):
        if mode == 'arrow':
            return pc.fill_null(x, 99)
        return 99 if x is None else x
    con.create_function('f', f, [int], int, type=mode, null_handling='special' if special else 'default')
    return con.sql('select f(x) from (values (1),(NULL),(2))t(x)').fetchall()


def fail(mode, return_null):
    con = duckdb.connect(config={'threads': 1})
    def f(x):
        raise ValueError('callback failure')
    con.create_function('f', f, [int], int, type=mode, exception_handling='return_null' if return_null else 'default')
    return con.sql('select f(i) from range(3)t(i)').fetchall()


def wrong_null(mode):
    con = duckdb.connect()
    def f(x):
        return pa.nulls(len(x)) if mode == 'arrow' else None
    con.create_function('f', f, [int], int, type=mode)
    return con.sql('select f(1)').fetchall()


def annotation():
    def f(x: int, y: int) -> int:
        return x + y
    con = duckdb.connect()
    con.create_function('f', f)
    return con.sql('select f(3,4)').fetchall()


def variadic():
    def f(*args) -> int:
        return len(args)
    con = duckdb.connect()
    con.create_function('f', f)
    return [con.sql('select f(1,2,3)').fetchall(), con.sql('select f()').fetchall()]


def transaction():
    con = duckdb.connect()
    con.execute('create table t(i int)')
    con.begin()
    con.execute('insert into t values (1)')
    con.create_function('f', lambda: 42, [], int)
    inside = con.sql('select f(), (select count(*) from t)').fetchall()
    con.rollback()
    rows = con.sql('select count(*) from t').fetchall()
    try:
        con.sql('select f()').fetchall()
        absent = False
    except duckdb.CatalogException:
        absent = True
    return [inside, rows, absent]


def settings():
    con = duckdb.connect()
    seen = []
    def f(x):
        seen.append(str(x.type))
        return x
    con.create_function('f', f, [str], str, type='arrow', side_effects=True)
    con.execute('set arrow_large_buffer_size=true')
    result = con.sql("select f('hello')").fetchall()
    return [result, seen]


def wrong_return(mode):
    con = duckdb.connect()
    def f(x):
        return pa.array(['bad'] * len(x)) if mode == 'arrow' else 'bad'
    con.create_function('f',f,[int],int,type=mode)
    return con.sql('select f(1)').fetchall()

def duplicate(collision):
    con=duckdb.connect();other=con.cursor()
    name='abs' if collision=='builtin' else 'f'
    if collision!='builtin':con.create_function(name,lambda x:x,[int],int)
    other.create_function(name,lambda x:x,[int],int)
    return 'registered'

for collision in ['builtin','connection']:
    capture('duplicate_'+collision,lambda collision=collision:duplicate(collision))
for mode in ['native','arrow']:
    capture(mode+'_wrong_return',lambda mode=mode:wrong_return(mode))

for mode in ['native', 'arrow']:
    capture(mode + '_lifecycle', lambda mode=mode: lifecycle(mode))
    for special in [False, True]:
        capture(f'{mode}_null_{special}', lambda mode=mode, special=special: nulls(mode, special))
    for return_null in [False, True]:
        capture(f'{mode}_exception_{return_null}', lambda mode=mode, return_null=return_null: fail(mode, return_null))
    capture(mode + '_wrong_null', lambda mode=mode: wrong_null(mode))
capture('annotation', annotation)
capture('variadic', variadic)
capture('transaction', transaction)
capture('arrow_settings', settings)
for key, value in [('type','wat'),('null_handling',2),('exception_handling','forward_error')]:
    capture('option_' + key, lambda key=key,value=value: duckdb.connect().create_function('f',lambda:1,[],int,**{key:value}))
for label,parameters,ret in [('count',[int],int),('list_required',(int,),int),('return_missing',[],None)]:
    capture('signature_'+label,lambda parameters=parameters,ret=ret:duckdb.connect().create_function('f',lambda:1,parameters,ret))
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2)+'\n')
print(json.dumps({'outcomes':len(outcomes)}))
