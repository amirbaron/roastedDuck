"""Native relation box rendering, configuration/cache and fetch independence."""
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import sys
import duckdb

outcomes = {}
def capture(name, action):
    try: outcomes[name] = {'value': action()}
    except Exception as error: outcomes[name] = {'error':type(error).__name__,'message':str(error)}

def render(query, options=None):
    con=duckdb.connect()
    rel=con.sql(query)
    if options is None: return [str(rel),repr(rel)]
    output=io.StringIO()
    with redirect_stdout(output): rel.show(**options)
    return output.getvalue()

queries={
 'simple':"select 42 a, 'hello' b, NULL::int c",
 'unicode':"select '雪😀é' a, 'line\nbreak' b, 'x\ty' c",
 'empty':'select 42 a where false',
 'nested':"select [1,NULL,3] a, {'x':1,'y':'b'} b, MAP(['a'],[2]) c",
 'decimals':"select 1234567890::bigint a, 12.345::decimal(6,3) b, 1.25::double c",
 'wide':"select repeat('long',40) a, repeat('x',40) b, 12 c",
 'many':'select i from range(30)t(i)',
}
for name,query in queries.items():capture(name,lambda query=query:render(query))
for name,options in {
 'width':{'max_width':80},'rows':{'max_rows':4},'column_width':{'max_col_width':6},
 'null':{'null_value':'missing'},'rows_mode':{'render_mode':'rows'},
 'columns_mode':{'render_mode':'columns'},'int_columns':{'render_mode':1},
 'enum_columns':{'render_mode':duckdb.RenderMode.COLUMNS},
 'bad_mode':{'render_mode':'bad'},'bad_int_mode':{'render_mode':9},
}.items():capture(name,lambda options=options:render('select i, NULL::varchar b from range(30)t(i)',options))

def cached():
 con=duckdb.connect();rel=con.sql('select i from range(30)t(i)'); first=str(rel)
 output=io.StringIO()
 with redirect_stdout(output):rel.show(max_rows=4)
 return [first,output.getvalue(),str(rel),rel.fetchone(),len(rel.fetchall())]
capture('cache_fetch',cached)

def closed():
 con=duckdb.connect();rel=con.sql('select 42');con.close();return str(rel)
capture('closed',closed)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes},indent=2)+'\n')
print(json.dumps({'outcomes':len(outcomes)}))
