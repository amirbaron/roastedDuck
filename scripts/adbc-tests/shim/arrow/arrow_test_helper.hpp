#pragma once
#include "test_helpers.hpp"
#include "duckdb/common/adbc/adbc.h"
#include "duckdb/common/arrow/arrow.hpp"
#include <unistd.h>
namespace duckdb {
class KeywordHelper {public:static string WriteOptionallyQuoted(const string &name){string out="\"";for(char c:name){out+=c;if(c=='"')out+=c;}return out+"\"";}};
class FileSystem {public:static string GetWorkingDirectory(){char buffer[8192];if(!getcwd(buffer,sizeof(buffer)))throw std::runtime_error("getcwd");return buffer;}};
class ResultValue {
public:
 ResultValue(string text,int64_t integer,bool null):text(std::move(text)),integer(integer),null(null){}
 const string &ToString()const{return text;}
 template<class T>T GetValue()const{return static_cast<T>(integer);}
 bool IsNull()const{return null;}
 bool operator==(int64_t other)const{return !null&&integer==other;}
private:string text;int64_t integer;bool null;
};
class CapiTestQueryResult {
public:
 explicit CapiTestQueryResult(duckdb_connection connection,const string &query):connection(connection),query(query),status(duckdb_query(connection,query.c_str(),&result)){}
 ~CapiTestQueryResult(){duckdb_destroy_result(&result);duckdb_destroy_result(&text_result);}
 bool HasError()const{return status!=DuckDBSuccess;}
 string GetError(){auto error=duckdb_result_error(&result);return error?error:"";}
 idx_t RowCount(){return duckdb_row_count(&result);}
 idx_t ColumnCount(){return duckdb_column_count(&result);}
 string ColumnName(idx_t column){auto name=duckdb_column_name(&result,column);return name?name:"";}
 duckdb_result &NativeResult(){return result;}
 ResultValue GetValue(idx_t column,idx_t row){
 auto source=&result;auto type=duckdb_column_type(&result,column);
 if(type==DUCKDB_TYPE_LIST||type==DUCKDB_TYPE_STRUCT||type==DUCKDB_TYPE_MAP||type==DUCKDB_TYPE_UNION||type==DUCKDB_TYPE_ARRAY){
  // Legacy result getters cannot fetch nested values. Apply DuckDB's actual VARCHAR
  // cast (the operation behind Value::ToString), rather than emulating its formatter.
  if(!text_ready){string body=query;while(!body.empty()&&isspace(static_cast<unsigned char>(body.back())))body.pop_back();if(!body.empty()&&body.back()==';')body.pop_back();string sql="WITH __adbc_text(";for(idx_t i=0;i<ColumnCount();i++){if(i)sql+=",";sql+="c"+std::to_string(i);}sql+=") AS ("+body+") SELECT ";for(idx_t i=0;i<ColumnCount();i++){if(i)sql+=",";sql+="CAST(c"+std::to_string(i)+" AS VARCHAR)";}sql+=" FROM __adbc_text";if(duckdb_query(connection,sql.c_str(),&text_result)!=DuckDBSuccess){auto message=duckdb_result_error(&text_result);throw std::runtime_error(message?message:"nested text result");}text_ready=true;}
  source=&text_result;
 }
 auto value=duckdb_value_varchar(source,column,row);string text=value?value:"NULL";duckdb_free(value);return ResultValue(text,duckdb_value_int64(&result,column,row),duckdb_value_is_null(source,column,row));
}
private:duckdb_connection connection;string query;duckdb_result result{},text_result{};bool text_ready=false;duckdb_state status;
};
using MaterializedQueryResult=CapiTestQueryResult;
#ifdef ADBC_RUST_TARGET
extern "C" duckdb_connection roastedduck_adbc_test_connection(AdbcConnection *);
extern "C" duckdb_database roastedduck_adbc_test_database(AdbcDatabase *);
inline duckdb_database NativeDatabase(AdbcDatabase *database){return roastedduck_adbc_test_database(database);}
inline duckdb_connection NativeConnection(AdbcConnection *connection){return roastedduck_adbc_test_connection(connection);}
#else
extern "C" duckdb_connection adbc_cpp_test_connection(AdbcConnection *);
extern "C" duckdb_connection adbc_cpp_test_connect(AdbcConnection *);
inline duckdb_connection NativeConnection(AdbcConnection *connection){return adbc_cpp_test_connection(connection);}

#endif
inline unique_ptr<MaterializedQueryResult> NativeQuery(AdbcConnection *connection,const string &query){return make_uniq<MaterializedQueryResult>(NativeConnection(connection),query);}

inline string SchemaSignature(const ArrowSchema &schema){
 string result=schema.format?schema.format:"";result+="|"+std::to_string(schema.flags)+"|"+(schema.name?schema.name:"");
 if(schema.metadata){const char *p=schema.metadata;int32_t count;memcpy(&count,p,4);p+=4;for(int32_t i=0;i<count;i++)for(int j=0;j<2;j++){int32_t length;memcpy(&length,p,4);p+=4+length;}result.append(schema.metadata,p-schema.metadata);}
 for(int64_t i=0;i<schema.n_children;i++){auto child=SchemaSignature(*schema.children[i]);result+=std::to_string(child.size())+":"+child;}if(schema.dictionary)result+="dict:"+SchemaSignature(*schema.dictionary);return result;
}
inline string LogicalSignature(duckdb_result &result,idx_t column){
 auto type=duckdb_column_logical_type(&result,column);auto options=duckdb_result_get_arrow_options(&result);const char *name="x";ArrowSchema schema{};auto error=duckdb_to_arrow_schema(options,&type,&name,1,&schema);string signature;
 if(error){auto text=duckdb_error_data_message(error);auto message=string(text?text:"");duckdb_destroy_error_data(&error);duckdb_destroy_logical_type(&type);duckdb_destroy_arrow_options(&options);throw std::runtime_error("type signature conversion: "+message);}else{signature=SchemaSignature(schema);schema.release(&schema);}duckdb_destroy_logical_type(&type);duckdb_destroy_arrow_options(&options);return signature;
}
inline bool NativeIsAutoCommit(AdbcConnection *connection){int64_t value=-1;AdbcError error{};auto code=connection->private_driver->ConnectionGetOptionInt(connection,ADBC_CONNECTION_OPTION_AUTOCOMMIT,&value,&error);if(error.release)error.release(&error);if(code!=ADBC_STATUS_OK)throw std::runtime_error("read autocommit");return value!=0;}
// duckdb_arrow_scan changes release on its private stream copy. Manager streams validate
// their release callback identity, so delegate through an independent borrowed proxy.
inline int ProxySchema(ArrowArrayStream *proxy,ArrowSchema *out){auto stream=static_cast<ArrowArrayStream *>(proxy->private_data);return stream->get_schema(stream,out);}
inline int ProxyNext(ArrowArrayStream *proxy,ArrowArray *out){auto stream=static_cast<ArrowArrayStream *>(proxy->private_data);return stream->get_next(stream,out);}
inline const char *ProxyError(ArrowArrayStream *proxy){auto stream=static_cast<ArrowArrayStream *>(proxy->private_data);return stream->get_last_error(stream);}
inline void ProxyRelease(ArrowArrayStream *proxy){proxy->release=nullptr;}
inline bool NativeArrowComparison(AdbcDatabase *database,AdbcConnection *connection,const string &query,ArrowArrayStream &stream){
 // Use the same independent database connection as upstream. SQL EXCEPT ALL retains
 // multiset, NULL and numeric comparison semantics; type signatures preserve nested parameters.
 duckdb_connection native=nullptr;
#ifdef ADBC_RUST_TARGET
 if(duckdb_connect(NativeDatabase(database),&native)!=DuckDBSuccess)return false;
#else
 native=adbc_cpp_test_connect(connection);if(!native)return false;
#endif
 ArrowArrayStream proxy{ProxySchema,ProxyNext,ProxyError,ProxyRelease,&stream};
 struct Guard {duckdb_connection &connection;~Guard(){duckdb_disconnect(&connection);}} guard{native};
 if(!stream.private_data){MaterializedQueryResult expected(native,query);return !expected.HasError()&&duckdb_result_statement_type(expected.NativeResult())==DUCKDB_STATEMENT_TYPE_INVALID;}
 if(duckdb_arrow_scan(native,"__adbc_roundtrip",reinterpret_cast<duckdb_arrow_stream>(&proxy))!=DuckDBSuccess)return false;
 MaterializedQueryResult materialize(native,"CREATE TEMP TABLE __adbc_actual AS SELECT * FROM __adbc_roundtrip");
 MaterializedQueryResult materialize_expected(native,"CREATE TEMP TABLE __adbc_expected AS "+query);
 if(materialize.HasError()||materialize_expected.HasError())return false;
 MaterializedQueryResult actual(native,"SELECT * FROM __adbc_actual"),expected(native,"SELECT * FROM __adbc_expected");
 if(actual.HasError()||expected.HasError()||actual.RowCount()!=expected.RowCount()||actual.ColumnCount()!=expected.ColumnCount())return false;
 for(idx_t col=0;col<expected.ColumnCount();col++)if(duckdb_column_type(&expected.NativeResult(),col)!=DUCKDB_TYPE_ENUM&&LogicalSignature(actual.NativeResult(),col)!=LogicalSignature(expected.NativeResult(),col))return false;
 MaterializedQueryResult difference(native,"SELECT * FROM __adbc_expected EXCEPT ALL SELECT * FROM __adbc_actual");return !difference.HasError()&&difference.RowCount()==0;
}
}
