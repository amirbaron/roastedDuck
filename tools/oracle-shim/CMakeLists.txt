cmake_minimum_required(VERSION 3.16)
project(slt_oracle LANGUAGES CXX)

# The shim links the reference DuckDB build; it is dev tooling and never part of a cargo build.
set(DUCKDB_SOURCE_DIR "" CACHE PATH "DuckDB 1.5.6 source tree")
set(DUCKDB_BUILD_DIR "" CACHE PATH "DuckDB build directory (e.g. <source>/build/release)")
if(NOT DUCKDB_SOURCE_DIR OR NOT DUCKDB_BUILD_DIR)
  message(FATAL_ERROR "Set -DDUCKDB_SOURCE_DIR=<duckdb source> and -DDUCKDB_BUILD_DIR=<duckdb build dir>")
endif()

find_library(DUCKDB_LIBRARY NAMES duckdb PATHS "${DUCKDB_BUILD_DIR}/src" NO_DEFAULT_PATH REQUIRED)

set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

add_library(slt_oracle SHARED slt_oracle.cpp)
target_include_directories(slt_oracle PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}"
  "${DUCKDB_SOURCE_DIR}/src/include"
  "${DUCKDB_SOURCE_DIR}/third_party/fmt/include"
  "${DUCKDB_SOURCE_DIR}/third_party/re2"
  "${DUCKDB_SOURCE_DIR}/third_party/fast_float"
  "${DUCKDB_SOURCE_DIR}/third_party/utf8proc/include")
target_compile_definitions(slt_oracle PRIVATE
  NDEBUG
  SLT_DUCKDB_BUILD_DIRECTORY="${DUCKDB_BUILD_DIR}")
target_link_libraries(slt_oracle PRIVATE "${DUCKDB_LIBRARY}")
set_target_properties(slt_oracle PROPERTIES
  BUILD_RPATH "${DUCKDB_BUILD_DIR}/src"
  INSTALL_RPATH "${DUCKDB_BUILD_DIR}/src")
