"""Native aggregate/window wrappers: values, metadata, generated SQL and error semantics."""
import json,math,sys
from pathlib import Path
import duckdb
outcomes={}
def clean(value):
 if isinstance(value,float) and math.isnan(value):return 'NaN'
 if isinstance(value,(tuple,list)):return [clean(v)for v in value]
 if isinstance(value,dict):return {str(k):clean(v)for k,v in value.items()}
 return value

def capture(name,action):
 try:outcomes[name]={'value':clean(action())}
 except Exception as error:outcomes[name]={'error':type(error).__name__,'message':str(error)}
def run(method,args,kwargs):
 con=duckdb.connect(config={'threads':1});rel=con.sql("select i, i%3 g, i%2=0 b, i::varchar s from range(1,9)t(i)");result=getattr(rel,method)(*args,**kwargs)
 return [result.type,result.columns,[str(t)for t in result.types],result.order('ALL').fetchall()]
for method in ['any_value','avg','mean','bit_and','bit_or','bit_xor','count','favg','first','fsum','geomean','histogram','list','last','max','min','product','sum','median','mode','stddev_pop','stddev_samp','stddev','std','var_pop','var_samp','variance','var']:
 capture(method,lambda method=method:run(method,('i',),{}))
for method in ['sum','avg','list','count']:
 capture(method+'_groups',lambda method=method:run(method,('i',),{'groups':'g','projected_columns':'g'}))
for method in ['bool_and','bool_or']:capture(method,lambda method=method:run(method,('b',),{}))
for method in ['arg_max','arg_min']:capture(method,lambda method=method:run(method,('s','i'),{}))
for name,method,args,kwargs in [
 ('bitstring','bitstring_agg',('i',),{'min':1,'max':8}),('bit_partial','bitstring_agg',('i',),{'min':1}),
 ('bit_type','bitstring_agg',('i',),{'min':'1','max':'8'}),('count_star','count',('',),{}),
 ('sum_multiple','sum',('i,g',),{}),('unique','unique',('g',),{}),('value_counts','value_counts',('g',),{}),
 ('apply','apply',('sum','i'),{}),('apply_parameter','apply',('quantile_cont','i'),{'function_parameter':'0.25'}),
 ('both_modes','sum',('i',),{'groups':'g','window_spec':'over()'}),
 ('quantile','quantile',('i',),{}),('quantile_disc','quantile_disc',('i',),{'q':0.333333333}),
 ('quantile_list','quantile_cont',('i',),{'q':[0.123456789,0.8]}),('quantile_none','quantile_cont',('i',),{'q':None}),
 ('quantile_int','quantile_cont',('i',),{'q':1}),('quantile_tuple','quantile_cont',('i',),{'q':(0.5,)}),
 ('string_agg','string_agg',('s',),{}),('string_sep','string_agg',('s',),{'sep':"a'b"}),
 ('string_alpha','string_agg',('s',),{'sep':'abc'}),('string_empty','string_agg',('s',),{'sep':''}),
]:capture(name,lambda method=method,args=args,kwargs=kwargs:run(method,args,kwargs))
for method in ['row_number','rank','dense_rank','rank_dense','percent_rank','cume_dist']:
 capture(method,lambda method=method:run(method,('over(partition by g order by i)',),{'projected_columns':'g,i'}))
for method in ['lag','lead','first_value','last_value','nth_value']:
 kwargs={'projected_columns':'g,i'}
 if method=='nth_value':kwargs['offset']=2
 capture(method,lambda method=method,kwargs=kwargs:run(method,('i','over(partition by g order by i)'),kwargs))
capture('ntile',lambda:run('n_tile',('over(order by i)',3),{'projected_columns':'i'}))
capture('lag_zero',lambda:run('lag',('i','over(order by i)'),{'offset':0}))
capture('lag_nulls',lambda:run('lag',('i','over(order by i)'),{'ignore_nulls':True}))
capture('sum_window',lambda:run('sum',('i',),{'window_spec':'over(partition by g)','projected_columns':'g,i'}))
def quoted(name):
 con=duckdb.connect();rel=con.sql('select 1 as "with space",2 as "a.b",3 as "雪"');result=rel.sum(name)
 return [result.columns,result.fetchall()]
for name in ['with space','"a.b"','雪']:capture('quoted_'+name,lambda name=name:quoted(name))
def trailing_unicode():
 con=duckdb.connect();rel=con.sql('select 11 as prefix,99 as "prefix雪"');return rel.sum('prefix雪').fetchall()
capture('trailing_unicode_wrong_column',trailing_unicode)
capture('quoted_unicode',lambda:quoted('"雪"'))
for group in ['i order by g','()','grouping sets(())','cube(g)','rollup(g)']:
 capture('group_'+group,lambda group=group:run('sum',('i',),{'groups':group}))

Path(sys.argv[1]).write_text(json.dumps({'version':duckdb.__version__,'outcomes':outcomes,'normalization':'NaN values only, retaining every error'},indent=2)+'\n');print(json.dumps({'outcomes':len(outcomes)}))
