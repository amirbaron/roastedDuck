"""Actual cross-engine file/WAL journey after native dataframe append."""
import datetime,decimal,json,os,sys
from pathlib import Path
import duckdb,pandas as pd
mode,path,output=sys.argv[1],Path(sys.argv[2]),Path(sys.argv[3])
schema="""create table typed_rows(
 i bigint,s varchar,raw blob,d decimal(18,2),t timestamp,f boolean,day date,
 l integer[] default [1,NULL,3],a integer[2] default [2,4],
 st struct(n integer,label varchar) default {'n':7,'label':'雪'},
 m map(varchar,integer) default map(['x','y'],[1,2]),
 u uuid default '01234567-89ab-cdef-0123-456789abcdef',
 b bit default '0101'::bit,span interval default '2 days',
 e enum('small','雪') default '雪')"""
query="""select i,s,hex(raw),d::varchar,t::varchar,f,day::varchar,l::varchar,a::varchar,
 st::varchar,m::varchar,u::varchar,b::varchar,span::varchar,e::varchar from typed_rows order by i"""
con=duckdb.connect(str(path))
if mode.startswith('write'):
 con.execute(schema);con.execute('checkpoint')
 frame=pd.DataFrame({'i':[1,2,3],'s':['hello',None,'雪\0e\u0301'],'raw':[b'\0\xff',None,b''],'d':[decimal.Decimal('-12.30'),None,decimal.Decimal('5.00')],'t':[datetime.datetime(2001,2,3,4,5,6,789012),None,datetime.datetime(1970,1,1)],'f':[True,None,False],'day':[datetime.date(2000,2,29),None,datetime.date(1970,1,1)]})
 con.append('typed_rows',frame,by_name=True)
 rows=con.sql(query).fetchall();assert len(rows)==3
 if mode=='write-checkpoint':con.execute('checkpoint')
 output.write_text(json.dumps({'mode':mode,'version':duckdb.__version__,'rows':rows,'columns':con.sql('from typed_rows').columns,'wal_exists':Path(str(path)+'.wal').exists()},indent=2,ensure_ascii=False)+'\n')
 if mode=='write-wal':os._exit(0)
 con.close()
elif mode=='read-write':
 rows=con.sql(query).fetchall();assert len(rows)==3
 con.execute("insert into typed_rows(i,s,raw,d,t,f,day) values(4,'other engine','abc'::blob,12.34,'2020-01-02 03:04:05',true,'2020-01-02')")
 after=con.sql(query).fetchall();assert len(after)==4
 con.execute('checkpoint');con.close();output.write_text(json.dumps({'mode':mode,'version':duckdb.__version__,'rows':rows,'after':after},indent=2,ensure_ascii=False)+'\n')
else:
 rows=con.sql(query).fetchall();assert len(rows)==4
 con.close();output.write_text(json.dumps({'mode':mode,'version':duckdb.__version__,'rows':rows},indent=2,ensure_ascii=False)+'\n')
