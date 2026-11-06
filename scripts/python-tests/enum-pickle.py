"""Native enum state interoperability using frozen pybind11's NEWOBJ protocol."""
import duckdb,json,pickle,pathlib,sys
mode,protocol,filename,report=sys.argv[1],int(sys.argv[2]),pathlib.Path(sys.argv[3]),pathlib.Path(sys.argv[4])
expected=[*duckdb.StatementType.__members__.values(),duckdb.StatementType(29),duckdb.StatementType(255),*duckdb.ExpectedResultType.__members__.values(),duckdb.ExpectedResultType(255)]
if mode=='write':filename.write_bytes(pickle.dumps(expected,protocol=protocol));values=expected
else:values=pickle.loads(filename.read_bytes());assert [repr(v)for v in values]==[repr(v)for v in expected]
report.write_text(json.dumps({'mode':mode,'protocol':protocol,'values':[repr(v)for v in values]},indent=2)+'\n')
