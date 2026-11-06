// Public ADBC cancellation with deterministic cross-thread handoff and no C++ engine internals.
#include "duckdb/common/adbc/adbc.h"
#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>
extern "C" AdbcStatusCode duckdb_adbc_init(int,void *,AdbcError *);
static void ok(AdbcStatusCode code,AdbcError &error){if(code)fprintf(stderr,"ADBC %u: %s\n",code,error.message?error.message:"");assert(code==ADBC_STATUS_OK);}
int main(){
 AdbcDriver driver{};AdbcError error{};AdbcDatabase database{};AdbcConnection connection{};ok(duckdb_adbc_init(ADBC_VERSION_1_1_0,&driver,&error),error);ok(driver.DatabaseNew(&database,&error),error);ok(driver.DatabaseInit(&database,&error),error);ok(driver.ConnectionNew(&connection,&error),error);ok(driver.ConnectionInit(&connection,&database,&error),error);
 for(bool statement_cancel:{false,true}){
  AdbcStatement statement{};ok(driver.StatementNew(&connection,&statement,&error),error);ok(driver.StatementSetSqlQuery(&statement,"SET threads=1",&error),error);ok(driver.StatementExecuteQuery(&statement,nullptr,nullptr,&error),error);ok(driver.StatementSetSqlQuery(&statement,"SELECT i FROM range(100000000) t(i)",&error),error);ArrowArrayStream stream{};ok(driver.StatementExecuteQuery(&statement,&stream,nullptr,&error),error);
  if(!statement_cancel)ok(driver.StatementRelease(&statement,&error),error);
  std::atomic<bool> ready{false},resume{false},done{false};int stream_result=0;std::string message;
  std::thread consumer([&](){for(;;){ArrowArray array{};int result=stream.get_next(&stream,&array);if(result){stream_result=result;auto error=stream.get_last_error(&stream);if(error)message=error;break;}if(!array.release)break;array.release(&array);if(!ready.exchange(true)){while(!resume.load())std::this_thread::yield();}}done=true;});
  for(int i=0;i<1000&&!ready.load()&&!done.load();i++)std::this_thread::sleep_for(std::chrono::milliseconds(5));
  bool started=ready.load();ok(statement_cancel?driver.StatementCancel(&statement,&error):driver.ConnectionCancel(&connection,&error),error);resume=true;consumer.join();assert(started&&stream_result!=0&&message.find("Interrupted!")!=std::string::npos);
  AdbcStatusCode status=0;auto rich=driver.ErrorFromArrayStream(&stream,&status);assert(rich&&driver.ErrorGetDetailCount(rich)==1);auto detail=driver.ErrorGetDetail(rich,0);assert(detail.value_length==9&&!memcmp(detail.value,"Interrupt",9));printf("cancel_%s=%u,Interrupt\n",statement_cancel?"statement":"connection",status);
  stream.release(&stream);if(statement_cancel)ok(driver.StatementRelease(&statement,&error),error);
  ok(driver.StatementNew(&connection,&statement,&error),error);ok(driver.StatementSetSqlQuery(&statement,"SELECT 42",&error),error);ok(driver.StatementExecuteQuery(&statement,&stream,nullptr,&error),error);ArrowArray array{};assert(stream.get_next(&stream,&array)==0&&array.length==1);array.release(&array);stream.release(&stream);ok(driver.StatementRelease(&statement,&error),error);
 }
 ok(driver.ConnectionRelease(&connection,&error),error);ok(driver.DatabaseRelease(&database,&error),error);return 0;
}
