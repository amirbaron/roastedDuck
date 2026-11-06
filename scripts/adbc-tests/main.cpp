// Catch runner plus an engine-identity check. The Rust run must not load a C++ DuckDB fallback.
#define main AdbcCatchMain
#include "../capi-tests/shim/main.cpp"
#undef main
#include <filesystem>
#if defined(__APPLE__)
#include <mach-o/dyld.h>
#endif
static bool CheckEngineImage(){
#if defined(__APPLE__)
 auto requested=std::getenv("DUCKDB_INSTALL_LIB");if(!requested)return false;size_t count=0;
 for(uint32_t i=0;i<_dyld_image_count();i++){auto name=_dyld_get_image_name(i);if(!name)continue;std::filesystem::path image(name);if(image.filename()=="libduckdb.dylib"){count++;if(!std::filesystem::equivalent(image,requested)){fprintf(stderr,"Unexpected DuckDB image: %s\n",name);return false;}}}
 if(count!=1){fprintf(stderr,"Expected one DuckDB engine image, got %zu\n",count);return false;}
 fprintf(stderr,"Verified DuckDB engine image: %s\n",std::filesystem::canonical(requested).c_str());
#endif
 return true;
}
int main(int argc,char **argv){if(!CheckEngineImage())return 100;int result=AdbcCatchMain(argc,argv);if(!CheckEngineImage())return 101;return result;}
