#pragma once
#include_next "duckdb/common/arrow/arrow.hpp"
#ifndef ARROW_FLAG_DICTIONARY_ORDERED
#define ARROW_FLAG_DICTIONARY_ORDERED 1
#define ARROW_FLAG_NULLABLE 2
#define ARROW_FLAG_MAP_KEYS_SORTED 4
#endif
