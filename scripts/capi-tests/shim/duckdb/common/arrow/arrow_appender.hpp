// Stand-in for DuckDB's arrow_appender.hpp: the C++ appender is only used by the
// excluded "test scan" section; the Arrow structs live in arrow.hpp.
#pragma once

#include "duckdb/common/arrow/arrow.hpp"
