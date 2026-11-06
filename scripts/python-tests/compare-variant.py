"""VARIANT host conversion across native scalar and heterogeneous nested values."""
import datetime,decimal,json,math,pathlib,sys,uuid
import duckdb
outcomes={};raw={}
def clean(v):
 if v is None:return None
 if isinstance(v,bool):return {'bool':v}
 if isinstance(v,int):return {'int':str(v)}
 if isinstance(v,float):return {'float':'nan'if math.isnan(v)else repr(v)}
 if isinstance(v,decimal.Decimal):return {'decimal':str(v)}
 if isinstance(v,bytes):return {'bytes':v.hex()}
 if isinstance(v,(datetime.datetime,datetime.date,datetime.time)):return {type(v).__name__:v.isoformat()}
 if isinstance(v,datetime.timedelta):return {'timedelta':[v.days,v.seconds,v.microseconds]}
 if isinstance(v,uuid.UUID):return {'uuid':str(v)}
 if isinstance(v,str):return {'str':v}
 if isinstance(v,dict):return {'dict':[[clean(k),clean(x)]for k,x in v.items()]}
 if isinstance(v,(list,tuple)):return {type(v).__name__:[clean(x)for x in v]}
 return {'other':repr(v)}
def capture(name,action):
 try:outcomes[name]={'value':clean(action())}
 except Exception as e:raw[name]={'error':type(e).__name__,'message':str(e)};outcomes[name]=raw[name]
values=['NULL','true','false','42::tinyint','-12345::smallint','42::integer','-9223372036854775808::bigint','170141183460469231731687303715884105727::hugeint','18446744073709551615::ubigint','340282366920938463463374607431768211455::uhugeint','1.25::float','1.25::double',"'nan'::double","'inf'::double","12.30::decimal(18,2)","'雪\\0a'",r"'\x00\xFF'::blob","'01234567-89ab-cdef-0123-456789abcdef'::uuid","'2000-02-29'::date","'12:34:56.123456'::time","'2001-02-03 04:05:06.123456'::timestamp","'1969-12-31 23:59:59.999999999'::timestamp_ns","'2001-02-03 04:05:06+02'::timestamptz","'2 months 3 days 4 seconds'::interval",'[1,NULL,3]',"{'x':[1,NULL],'y':{'a':'雪'}}","map(['x','y'],[1,2])","[42::variant,'雪'::variant,true::variant,NULL::variant]"]
for value in values:
 for method in ['fetchall','fetchnumpy','df']:
  def run(value=value,method=method):
   c=duckdb.connect(config={'TimeZone':'UTC'});r=c.sql(f'select ({value})::variant v');r=getattr(r,method)();return r['v'].tolist()if method!='fetchall'else r
  capture(method+'_'+value,run)
def rows():
 c=duckdb.connect();return c.sql("select i::variant from range(5000)t(i)").fetchall()
capture('rows5000',lambda:[len(rows()),rows()[0],rows()[-1]])
pathlib.Path(sys.argv[1]).write_text(json.dumps({'outcomes':outcomes,'raw_errors':raw},indent=2,ensure_ascii=False)+'\n');print(json.dumps({'cases':len(outcomes)}))
