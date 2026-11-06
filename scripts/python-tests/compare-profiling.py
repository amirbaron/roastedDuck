"""Native profiling formats/state; raw metric values retained alongside stable comparisons."""
import json,re,sys
from pathlib import Path
import duckdb
outcomes={};profiles={}
def capture(name,action):
 try:outcomes[name]={'value':action()}
 except Exception as e:outcomes[name]={'error':type(e).__name__,'message':str(e)}
def stable_profile(name,con):
 d=json.loads(con.get_profiling_information('json'));profiles[name]=d
 volatile={'operator_timing','cpu_time','latency','blocked_thread_time','system_peak_buffer_memory','system_peak_temp_dir_size','total_memory_allocated','cumulative_cpu_time'}
 def clean(v):
  if isinstance(v,dict):return {k:clean(x)for k,x in v.items()if k not in volatile and not k.startswith(('planner','optimizer','physical_planner','cumulative_optimizer'))}
  if isinstance(v,list):return [clean(x)for x in v]
  return v
 return clean(d)
con=duckdb.connect(config={'threads':1})
for mode in ['json','query_tree','query_tree_optimizer','no_output','html','graphviz','JSON','mermaid','']:
 capture('disabled_'+mode,lambda mode=mode:con.get_profiling_information(mode))
capture('enable',con.enable_profiling)
for mode in ['json','query_tree','query_tree_optimizer','no_output','html','graphviz']:
 capture('empty_'+mode,lambda mode=mode:con.get_profiling_information(mode))
for name,sql in [('constant','SELECT 42 AS answer'),('aggregate','SELECT sum(i) s FROM range(1000)t(i)'),('empty_rows','SELECT i FROM range(0)t(i)'),('unicode','SELECT 42 AS "雪"')]:
 def query(name=name,sql=sql):
  rows=con.execute(sql).fetchall();return [rows,stable_profile(name,con)]
 capture('query_'+name,query)
capture('configured_before',lambda:con.sql("select current_setting('enable_profiling')").fetchall())
for mode in ['query_tree','query_tree_optimizer','no_output','html','graphviz']:
 def formatted(mode=mode):
  text=con.get_profiling_information(mode)
  if mode.startswith('query_tree'):text=re.sub(r'Total Time: [0-9.e+-]+s','Total Time: <time>',text);text=re.sub(r'\b[0-9]+\.[0-9]+s\b','<time>',text)
  return text
 capture('format_'+mode,formatted)
capture('configured_after',lambda:con.sql("select current_setting('enable_profiling')").fetchall())
capture('disable',con.disable_profiling);capture('disabled_after_query',lambda:con.get_profiling_information('json'))
capture('reenable',con.enable_profiling);capture('retained_after_enable',lambda:stable_profile('retained',con))
con.disable_profiling();con.execute('SELECT i FROM range(10000)t(i)');first=con.fetchone();capture('toggle_preserves_result',lambda:[first,con.enable_profiling(),con.disable_profiling(),con.fetchmany(2)])
con.close()
for name,action in [('get_closed',lambda:con.get_profiling_information()),('bad_format_closed',lambda:con.get_profiling_information('bad')),('enable_closed',con.enable_profiling),('disable_closed',con.disable_profiling)]:capture(name,action)
def module():
 c=duckdb.connect();duckdb.enable_profiling(connection=c);result=c.sql('SELECT 42').fetchall();d=json.loads(duckdb.get_profiling_information(connection=c));duckdb.disable_profiling(connection=c);return [result,d['query_name'],json.loads(duckdb.get_profiling_information(connection=c))]
capture('module_connection',module)
Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'raw_profiles':profiles,'normalization':'timing and resource metrics excluded only from functional comparisons; complete raw metric snapshots retained; query tree wall times replaced'},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
