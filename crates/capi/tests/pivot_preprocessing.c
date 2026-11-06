/* Public C API regression: dynamic PIVOT expansion before extracted-statement preparation. */
#include "duckdb.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>
int main(void){
 duckdb_database database=NULL;duckdb_connection connection=NULL;duckdb_result result;
 assert(duckdb_open(NULL,&database)==DuckDBSuccess);assert(duckdb_connect(database,&connection)==DuckDBSuccess);
 assert(duckdb_query(connection,"CREATE TABLE flights(city VARCHAR,carrier VARCHAR);INSERT INTO flights VALUES ('x','A'),('x','B'),('y','A')",&result)==DuckDBSuccess);duckdb_destroy_result(&result);
 const char *sql="PIVOT flights ON carrier USING count(*) GROUP BY city";duckdb_extracted_statements extracted=NULL;idx_t count=duckdb_extract_statements(connection,sql,&extracted);assert(count==2);assert(!duckdb_extract_statements_error(extracted));
 for(idx_t i=0;i<count;i++){
  duckdb_prepared_statement prepared=NULL;duckdb_state status=duckdb_prepare_extracted_statement(connection,extracted,i,&prepared);if(status!=DuckDBSuccess)fprintf(stderr,"prepare %llu: %s\n",(unsigned long long)i,duckdb_prepare_error(prepared));assert(status==DuckDBSuccess);
  assert(duckdb_execute_prepared(prepared,&result)==DuckDBSuccess);
  if(i+1==count){assert(duckdb_column_count(&result)==3);assert(duckdb_row_count(&result)==2);assert(!strcmp(duckdb_column_name(&result,1),"A"));assert(!strcmp(duckdb_column_name(&result,2),"B"));}
  duckdb_destroy_result(&result);duckdb_destroy_prepare(&prepared);
 }
 duckdb_destroy_extracted(&extracted);duckdb_disconnect(&connection);duckdb_close(&database);puts("dynamic PIVOT extracted/prepared/executed=pass");return 0;
}
