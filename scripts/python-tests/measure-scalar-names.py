"""Result-checked scalar/operator/group/UDF workloads with paired time and RSS."""
import gc
import json
import resource
import sys
import time
import duckdb

mode = sys.argv[1]
con = duckdb.connect(config={'threads': 1})
if mode == 'math':
    rows = 10_000_000
    sql = f'SELECT sum(abs(i-{rows//2})*3+5) FROM range({rows})t(i)'
    expected = [(3*(rows//2)**2+5*rows,)]
elif mode == 'group':
    rows = 1_000_000
    sql = f'SELECT i%100 k,count(*),sum(i+1) FROM range({rows})t(i) GROUP BY k ORDER BY k'
    expected = [(k, rows//100, (rows//100)*(k+1)+100*(rows//100)*(rows//100-1)//2) for k in range(100)]
elif mode == 'udf':
    rows = 100_000
    con.create_function('scalar_probe', lambda value: value+1, [int], int)
    sql = f'SELECT sum(scalar_probe(i)) FROM range({rows})t(i)'
    expected = [(rows*(rows+1)//2,)]
else:
    raise ValueError(mode)
assert con.execute('SELECT 42').fetchall() == [(42,)]
gc.collect()
before = resource.getrusage(resource.RUSAGE_SELF)
start = time.perf_counter()
result = con.execute(sql).fetchall()
elapsed = time.perf_counter()-start
after = resource.getrusage(resource.RUSAGE_SELF)
assert result == expected, (result, expected)
con.close()
print(json.dumps({'mode':mode,'rows':rows,'elapsed_seconds':elapsed,
                  'cpu_seconds':after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,
                  'peak_rss_bytes':after.ru_maxrss,'baseline_peak_rss_bytes':before.ru_maxrss,
                  'result_checked':True,'conditions':'freshmacOSprocess,signedRustdevbefore/after,threads1;notfinalreleaseacceptance'}))
