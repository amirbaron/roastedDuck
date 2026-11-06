/* Native names through duckdb.h: result/prepared/type metadata and a table callback. */
#include "duckdb.h"
#include <assert.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
static const char *column_name;
static void table_bind(duckdb_bind_info info) {
    duckdb_logical_type type=duckdb_create_logical_type(DUCKDB_TYPE_INTEGER);
    duckdb_bind_add_result_column(info,column_name,type);duckdb_destroy_logical_type(&type);
}
static void table_init(duckdb_init_info info) {
    int *done=malloc(sizeof(int));assert(done);*done=0;duckdb_init_set_init_data(info,done,free);
}
static void table_scan(duckdb_function_info info,duckdb_data_chunk output) {
    int *done=duckdb_function_get_init_data(info);
    if(*done){duckdb_data_chunk_set_size(output,0);return;}
    *done=1;int32_t *data=duckdb_vector_get_data(duckdb_data_chunk_get_vector(output,0));data[0]=42;duckdb_data_chunk_set_size(output,1);
}
static void identifier(char *out,const char *name) {
    *out++='"';for(;*name;name++){*out++=*name;if(*name=='"')*out++='"';}*out++='"';*out=0;
}
int main(void) {
    const char *names[]={"plain","caf\xe9","a\x80","a\xff","\xe9\x9b\xaa","a\"b","a\nb"};
    duckdb_database database=NULL;duckdb_connection connection=NULL;assert(duckdb_open(NULL,&database)==DuckDBSuccess);assert(duckdb_connect(database,&connection)==DuckDBSuccess);
    duckdb_table_function function=duckdb_create_table_function();duckdb_table_function_set_name(function,"native_name_callback");duckdb_table_function_set_bind(function,table_bind);duckdb_table_function_set_init(function,table_init);duckdb_table_function_set_function(function,table_scan);assert(duckdb_register_table_function(connection,function)==DuckDBSuccess);duckdb_destroy_table_function(&function);
    for(size_t i=0;i<sizeof(names)/sizeof(names[0]);i++) {
        const char *name=names[i];char quoted[128],sql[256];identifier(quoted,name);snprintf(sql,sizeof(sql),"SELECT 42 AS %s",quoted);
        duckdb_result result;assert(duckdb_query(connection,sql,&result)==DuckDBSuccess);assert(strcmp(duckdb_column_name(&result,0),name)==0);assert(duckdb_value_int32(&result,0,0)==42);duckdb_destroy_result(&result);
        duckdb_prepared_statement prepared=NULL;snprintf(sql,sizeof(sql),"SELECT CAST($%s AS INTEGER)",quoted);assert(duckdb_prepare(connection,sql,&prepared)==DuckDBSuccess);
        const char *parameter=duckdb_parameter_name(prepared,1);assert(parameter && strcmp(parameter,name)==0);duckdb_free((void*)parameter);idx_t index=0;assert(duckdb_bind_parameter_index(prepared,&index,name)==DuckDBSuccess);assert(index==1);assert(duckdb_bind_int32(prepared,index,42)==DuckDBSuccess);assert(duckdb_execute_prepared(prepared,&result)==DuckDBSuccess);assert(duckdb_value_int32(&result,0,0)==42);duckdb_destroy_result(&result);duckdb_destroy_prepare(&prepared);
        duckdb_logical_type type=duckdb_create_logical_type(DUCKDB_TYPE_INTEGER);duckdb_logical_type nested=duckdb_create_struct_type(&type,&name,1);char *member=duckdb_struct_type_child_name(nested,0);assert(member && strcmp(member,name)==0);duckdb_free(member);duckdb_destroy_logical_type(&nested);
        nested=duckdb_create_union_type(&type,&name,1);member=duckdb_union_type_member_name(nested,0);assert(member && strcmp(member,name)==0);duckdb_free(member);duckdb_destroy_logical_type(&nested);
        duckdb_logical_type_set_alias(type,name);member=duckdb_logical_type_get_alias(type);assert(member && strcmp(member,name)==0);duckdb_free(member);duckdb_destroy_logical_type(&type);
        snprintf(sql,sizeof(sql),"CREATE TABLE %s (%s INTEGER)",quoted,quoted);assert(duckdb_query(connection,sql,&result)==DuckDBSuccess);duckdb_destroy_result(&result);
        duckdb_table_description description=NULL;assert(duckdb_table_description_create(connection,NULL,name,&description)==DuckDBSuccess);member=duckdb_table_description_get_column_name(description,0);assert(member && strcmp(member,name)==0);duckdb_free(member);duckdb_table_description_destroy(&description);
        duckdb_appender appender=NULL;assert(duckdb_appender_create(connection,NULL,name,&appender)==DuckDBSuccess);assert(duckdb_appender_add_column(appender,name)==DuckDBSuccess);assert(duckdb_append_int32(appender,42)==DuckDBSuccess);assert(duckdb_appender_end_row(appender)==DuckDBSuccess);assert(duckdb_appender_destroy(&appender)==DuckDBSuccess);
        char alias[128],raw_alias[96];snprintf(raw_alias,sizeof(raw_alias),"src_%s",name);identifier(alias,raw_alias);snprintf(sql,sizeof(sql),"INSERT INTO %s SELECT %s FROM %s",quoted,quoted,alias);type=duckdb_create_logical_type(DUCKDB_TYPE_INTEGER);assert(duckdb_appender_create_query(connection,sql,1,&type,raw_alias,&name,&appender)==DuckDBSuccess);duckdb_destroy_logical_type(&type);assert(duckdb_append_int32(appender,7)==DuckDBSuccess);assert(duckdb_appender_end_row(appender)==DuckDBSuccess);assert(duckdb_appender_destroy(&appender)==DuckDBSuccess);
        snprintf(sql,sizeof(sql),"SELECT sum(%s)::BIGINT FROM %s",quoted,quoted);assert(duckdb_query(connection,sql,&result)==DuckDBSuccess);assert(duckdb_value_int64(&result,0,0)==49);duckdb_destroy_result(&result);snprintf(sql,sizeof(sql),"DROP TABLE %s",quoted);assert(duckdb_query(connection,sql,&result)==DuckDBSuccess);duckdb_destroy_result(&result);
        column_name=name;assert(duckdb_query(connection,"SELECT * FROM native_name_callback()",&result)==DuckDBSuccess);assert(strcmp(duckdb_column_name(&result,0),name)==0);assert(duckdb_value_int32(&result,0,0)==42);duckdb_destroy_result(&result);
    }
    duckdb_disconnect(&connection);duckdb_close(&database);puts("7 native-name journeys passed: result, prepare/bind/execute, STRUCT/UNION/alias, table callback, table description, appenders");return 0;
}
