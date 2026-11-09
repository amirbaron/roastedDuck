// Test-only native forwarding for upstream calls to private C++ ADBC functions.
#pragma once
#include "test_helpers.hpp"
#include "duckdb/common/adbc/adbc.h"
extern "C" AdbcStatusCode duckdb_adbc_init(int,void *,AdbcError *);
inline void HarnessReleaseError(AdbcError *error);
#ifdef ADBC_RUST_TARGET
extern "C" void roastedduck_adbc_test_initialize_error(AdbcError *);
#else
extern "C" void adbc_cpp_test_initialize_error(AdbcError *);
#endif
namespace duckdb_adbc {
inline AdbcDriver &NativeDriver(){static AdbcDriver driver=[](){AdbcDriver driver{};if(duckdb_adbc_init(ADBC_VERSION_1_1_0,&driver,nullptr)!=ADBC_STATUS_OK)throw std::runtime_error("initialize direct driver");return driver;}();return driver;}
inline void HarnessInitializeADBCError(AdbcError *error){if(!error)return;if(error->release==HarnessReleaseError)error->release(error);
#ifdef ADBC_RUST_TARGET
 roastedduck_adbc_test_initialize_error(error);
#else
 adbc_cpp_test_initialize_error(error);
#endif
}

inline AdbcStatusCode DatabaseInit(AdbcDatabase *v,AdbcError *e){return NativeDriver().DatabaseInit(v,e);}
inline AdbcStatusCode DatabaseRelease(AdbcDatabase *v,AdbcError *e){return NativeDriver().DatabaseRelease(v,e);}
inline AdbcStatusCode ConnectionRelease(AdbcConnection *v,AdbcError *e){return NativeDriver().ConnectionRelease(v,e);}
inline AdbcStatusCode StatementBindStream(AdbcStatement *v,ArrowArrayStream *s,AdbcError *e){return NativeDriver().StatementBindStream(v,s,e);}
inline AdbcStatusCode StatementExecuteQuery(AdbcStatement *v,ArrowArrayStream *s,int64_t *r,AdbcError *e){return NativeDriver().StatementExecuteQuery(v,s,r,e);}
inline AdbcStatusCode StatementSetOption(AdbcStatement *v,const char *k,const char *s,AdbcError *e){return NativeDriver().StatementSetOption(v,k,s,e);}
}
inline void HarnessReleaseError(AdbcError *error){if(error){delete[] error->message;error->message=nullptr;error->release=nullptr;}}
inline void SetError(AdbcError *error,const std::string &message){if(!error)return;std::string value;if(error->message){value=error->message;value+='\n';if(error->release)error->release(error);else delete[] error->message;}value+=message;error->message=new char[value.size()+1];memcpy(error->message,value.c_str(),value.size()+1);error->release=HarnessReleaseError;}
