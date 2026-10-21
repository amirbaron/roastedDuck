// Stand-in for DuckDB's arrow_converter.hpp: the C++ converter is only used by the
// excluded "test scan" section; the Arrow structs live in arrow.hpp.
#pragma once

#include "duckdb/common/arrow/arrow.hpp"
