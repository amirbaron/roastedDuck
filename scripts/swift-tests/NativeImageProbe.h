#pragma once
#include <stddef.h>

const char *rustdb_test_function_image(const void *address);
size_t rustdb_test_image_count(void);
const char *rustdb_test_image_name(size_t index);
